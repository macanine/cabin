//! Persistent IPv4-only networking inside the guest.
//!
//! The Mac gateway remains the enforcement boundary. This transaction also
//! disables IPv6 on guest interfaces and on interfaces created later, while
//! preserving the local loopback used by applications.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::context::Context;
use crate::error::{Error, Result};
use crate::model::OperationState;
use crate::ops::{Operation, OperationManager, Step, StepCtx};
use crate::system::{CommandSpec, privileged};

const CONFIG: &str = "/etc/sysctl.d/zz-cabin-ipv4.conf";
const CONF: &str = "/proc/sys/net/ipv6/conf";
const CONTENT: &str = "# Managed by cabin: guest network interfaces are IPv4-only.\nnet.ipv6.conf.default.disable_ipv6 = 1\nnet.ipv6.conf.all.disable_ipv6 = 1\nnet.ipv6.conf.lo.disable_ipv6 = 0\n";

fn interfaces(path: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Error::path("read IPv6 interface settings", path, error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::invalid("Invalid network interface name"))?;
        if name.is_empty() || name.contains('/') {
            return Err(Error::invalid("Invalid network interface name"));
        }
        if entry.path().join("disable_ipv6").exists() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn key(name: &str) -> String {
    // Slash notation keeps dots in interface names literal.
    format!("net/ipv6/conf/{name}/disable_ipv6")
}

fn read_values(ctx: &Context, conf: &Path) -> Result<Vec<(String, String)>> {
    interfaces(conf)?
        .into_iter()
        .map(|name| {
            let value = ctx.system.check(
                &CommandSpec::new(["sysctl", "-n", &key(&name)]).timeout(Duration::from_secs(5)),
            )?;
            let value = value.trim();
            if !matches!(value, "0" | "1") {
                return Err(Error::invalid(format!(
                    "Unexpected IPv6 setting for {name}"
                )));
            }
            Ok((name, value.to_string()))
        })
        .collect()
}

fn write_value(ctx: &Context, name: &str, value: &str) -> Result<()> {
    ctx.system.check(
        &privileged([
            "sysctl".to_string(),
            "-w".into(),
            format!("{}={value}", key(name)),
        ])
        .timeout(Duration::from_secs(5)),
    )?;
    Ok(())
}

fn desired(name: &str) -> &'static str {
    if name == "lo" { "0" } else { "1" }
}

struct Configure {
    ctx: Context,
    config: PathBuf,
    conf: PathBuf,
    previous: Vec<(String, String)>,
    backup: Option<PathBuf>,
    staged: PathBuf,
    touched: bool,
}

impl Step for Configure {
    fn name(&self) -> String {
        "Configure IPv4-only guest networking".into()
    }

    fn prepare(&mut self, _: &StepCtx<'_>) -> Result<()> {
        match fs::symlink_metadata(&self.config) {
            Ok(meta) if !meta.file_type().is_file() => {
                return Err(Error::invalid(
                    "The IPv4 configuration path is not a regular file",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::path(
                    "inspect IPv4 configuration",
                    &self.config,
                    error,
                ));
            }
        }
        self.previous = read_values(&self.ctx, &self.conf)?;
        self.backup = crate::fsx::backup(&self.config, &self.ctx.paths.backups().join("network"))?;
        crate::fsx::atomic_write(&self.staged, CONTENT.as_bytes(), Some(0o600))?;
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        self.touched = true;
        self.ctx.system.check(
            &privileged([
                "install".to_string(),
                "-m".into(),
                "0644".into(),
                self.staged.display().to_string(),
                self.config.display().to_string(),
            ])
            .timeout(Duration::from_secs(10)),
        )?;
        ctx.log("Installed persistent IPv4-only interface configuration");
        for name in ["default", "all", "lo"] {
            if self.previous.iter().any(|(item, _)| item == name) {
                write_value(&self.ctx, name, desired(name))?;
            }
        }
        // New interfaces inherit default=1; explicitly reapply to interfaces
        // that already existed when this operation began.
        for (name, _) in &self.previous {
            if !matches!(name.as_str(), "default" | "all" | "lo") {
                write_value(&self.ctx, name, "1")?;
            }
        }
        Ok(())
    }

    fn verify(&mut self, _: &StepCtx<'_>) -> Result<()> {
        if fs::read_to_string(&self.config).ok().as_deref() != Some(CONTENT) {
            return Err(Error::Internal(
                "Could not verify persistent IPv4 configuration".into(),
            ));
        }
        for (name, value) in read_values(&self.ctx, &self.conf)? {
            if value != desired(&name) {
                return Err(Error::Internal(format!("IPv6 is still enabled on {name}")));
            }
        }
        Ok(())
    }

    fn rollback(&mut self, _: &StepCtx<'_>) -> Result<()> {
        if !self.touched {
            return Ok(());
        }
        match &self.backup {
            Some(backup) => {
                self.ctx.system.check(
                    &privileged([
                        "install".to_string(),
                        "-m".into(),
                        "0644".into(),
                        backup.display().to_string(),
                        self.config.display().to_string(),
                    ])
                    .timeout(Duration::from_secs(10)),
                )?;
            }
            None => {
                self.ctx.system.check(
                    &privileged([
                        "rm".to_string(),
                        "-f".into(),
                        "--".into(),
                        self.config.display().to_string(),
                    ])
                    .timeout(Duration::from_secs(10)),
                )?;
            }
        }
        // Restore all first (it changes individual interfaces), then restore
        // every recorded interface and the default individually.
        if let Some((name, value)) = self.previous.iter().find(|(name, _)| name == "all") {
            write_value(&self.ctx, name, value)?;
        }
        for (name, value) in &self.previous {
            if name != "all" {
                write_value(&self.ctx, name, value)?;
            }
        }
        Ok(())
    }
}

fn ensure_at(ctx: &Context, ops: &OperationManager, config: &Path, conf: &Path) -> Result<()> {
    let regular = fs::symlink_metadata(config).is_ok_and(|meta| meta.file_type().is_file());
    if regular
        && fs::read_to_string(config).ok().as_deref() == Some(CONTENT)
        && read_values(ctx, conf)?
            .iter()
            .all(|(name, value)| value == desired(name))
    {
        return Ok(());
    }
    let record = ops.run(
        Operation::new("network.ipv4", "Configure IPv4-only guest networking")
            .key("guest-network-ipv4")
            .step(Configure {
                ctx: ctx.clone(),
                config: config.to_owned(),
                conf: conf.to_owned(),
                previous: Vec::new(),
                backup: None,
                staged: ctx.paths.data().join("network/ipv4-sysctl.conf"),
                touched: false,
            }),
    )?;
    if record.state != OperationState::Succeeded {
        let details = record
            .steps
            .iter()
            .flat_map(|step| step.log.iter())
            .filter(|line| line.contains("rollback") || line.contains("Rollback"))
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        let error = record
            .error
            .unwrap_or_else(|| "Could not configure IPv4-only guest networking".into());
        return Err(Error::Unavailable(if details.is_empty() {
            error
        } else {
            format!("{error}; {details}")
        }));
    }
    Ok(())
}

pub fn ensure(ctx: &Context, ops: &OperationManager) -> Result<()> {
    ensure_at(ctx, ops, Path::new(CONFIG), Path::new(CONF))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::AppPaths;
    use crate::store::Store;
    use crate::system::{Output, System};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Fake {
        values: Mutex<HashMap<String, String>>,
        fail: Mutex<bool>,
    }

    impl System for Fake {
        fn run(&self, spec: &CommandSpec) -> Result<Output> {
            let argv = &spec.argv;
            let index = argv
                .iter()
                .position(|arg| matches!(arg.as_str(), "sysctl" | "install" | "rm"))
                .unwrap();
            let args = &argv[index..];
            let mut stdout = String::new();
            let mut code = 0;
            match args[0].as_str() {
                "sysctl" if args[1] == "-n" => {
                    let values = self.values.lock().unwrap();
                    stdout = values.get(&args[2]).cloned().unwrap_or_default();
                }
                "sysctl" => {
                    let (key, value) = args[2].split_once('=').unwrap();
                    let mut fail = self.fail.lock().unwrap();
                    if *fail {
                        *fail = false;
                        code = 1;
                    } else {
                        let mut values = self.values.lock().unwrap();
                        if key == super::key("all") {
                            for (name, existing) in values.iter_mut() {
                                if name != &super::key("default") {
                                    *existing = value.into();
                                }
                            }
                        }
                        values.insert(key.into(), value.into());
                    }
                }
                "install" => {
                    fs::copy(&args[3], &args[4])?;
                }
                "rm" => {
                    let _ = fs::remove_file(&args[3]);
                }
                _ => unreachable!(),
            }
            Ok(Output {
                code: Some(code),
                stdout,
                stderr: String::new(),
            })
        }
    }

    fn fixture(dir: &Path) -> Result<(Context, OperationManager, Arc<Fake>, PathBuf, PathBuf)> {
        let fake = Arc::new(Fake::default());
        let conf = dir.join("proc");
        for name in ["all", "default", "lo", "data0"] {
            fs::create_dir_all(conf.join(name))?;
            fs::write(conf.join(name).join("disable_ipv6"), "")?;
            fake.values.lock().unwrap().insert(key(name), "0".into());
        }
        let config = dir.join("ipv4.conf");
        let store = Arc::new(Store::in_memory()?);
        let ctx = Context {
            paths: AppPaths::new(dir.join("data"), dir.join("home")),
            store: store.clone(),
            system: fake.clone(),
        };
        Ok((ctx, OperationManager::new(store), fake, config, conf))
    }

    #[test]
    fn applies_and_reuses_ipv4_configuration() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (ctx, ops, fake, config, conf) = fixture(dir.path())?;
        ensure_at(&ctx, &ops, &config, &conf)?;
        assert_eq!(fs::read_to_string(&config)?, CONTENT);
        let values = fake.values.lock().unwrap();
        for name in ["all", "default", "data0"] {
            assert_eq!(values[&key(name)], "1");
        }
        assert_eq!(values[&key("lo")], "0");
        drop(values);
        let count = ops.recent(10)?.len();
        ensure_at(&ctx, &ops, &config, &conf)?;
        assert_eq!(ops.recent(10)?.len(), count);
        fs::create_dir_all(conf.join("new0"))?;
        fs::write(conf.join("new0/disable_ipv6"), "")?;
        fake.values.lock().unwrap().insert(key("new0"), "1".into());
        ensure_at(&ctx, &ops, &config, &conf)?;
        assert_eq!(ops.recent(10)?.len(), count);
        Ok(())
    }

    #[test]
    fn ipv6_disabled_kernel_and_symlinked_config_are_handled() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (ctx, ops, _, config, conf) = fixture(dir.path())?;
        fs::remove_dir_all(&conf)?;
        ensure_at(&ctx, &ops, &config, &conf)?;
        assert_eq!(fs::read_to_string(&config)?, CONTENT);
        fs::remove_file(&config)?;
        let other = dir.path().join("other");
        fs::write(&other, CONTENT)?;
        std::os::unix::fs::symlink(&other, &config)?;
        assert!(ensure_at(&ctx, &ops, &config, &conf).is_err());
        assert_eq!(fs::read_to_string(&other)?, CONTENT);
        Ok(())
    }

    #[test]
    fn failed_apply_restores_file_and_interface_settings() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (ctx, ops, fake, config, conf) = fixture(dir.path())?;
        fs::write(&config, "old settings\n")?;
        *fake.fail.lock().unwrap() = true;
        assert!(ensure_at(&ctx, &ops, &config, &conf).is_err());
        assert_eq!(fs::read_to_string(&config)?, "old settings\n");
        assert!(
            fake.values
                .lock()
                .unwrap()
                .values()
                .all(|value| value == "0")
        );
        Ok(())
    }
}
