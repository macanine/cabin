//! Install Chrome on an existing machine, from the Chrome page.
//!
//! Chrome is not an installer option. Setup runs in place on the machine's
//! disk (the scripts are idempotent, so a retry or a repair just runs them
//! again); there are no disk snapshots. If setup fails, the host-side records
//! it changed (instance configuration, step states, markers) are put back so
//! the machine starts exactly as it was configured before, and a desktop that
//! the machine did not have before is switched off again in the guest.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::Config;
use crate::journal::state_path;
use crate::steps::base::{Step, StepContext};
use crate::steps::browser::BrowserStep;
use crate::steps::desktop::DesktopStep;
use crate::util::{atomic_write_bytes, atomic_write_json, message, now_millis};
use crate::vm::{Boot, Runtime};
use crate::{Result, browser, desktop};

/// Left behind by versions that kept a recovery snapshot; removed on sight.
pub fn legacy_transaction_path(cfg: &Config) -> PathBuf {
    cfg.instance_file("chrome-transaction.json")
}

/// The newest Chrome setup log for this machine, if setup has run at least once.
pub fn install_log_path(cfg: &Config) -> Option<PathBuf> {
    std::fs::read_dir(cfg.instance_logs_dir())
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            (name.starts_with("chrome-install-") && name.ends_with(".log"))
                .then(|| (name, entry.path()))
        })
        .max_by_key(|(name, _)| name.clone())
        .map(|(_, path)| path)
}

/// Host files Chrome setup changes, with their contents before it started.
struct Backup(Vec<(PathBuf, Option<Vec<u8>>)>);

impl Backup {
    fn take(cfg: &Config) -> Result<Self> {
        let paths = [
            cfg.instance_meta_path(),
            cfg.desktop_marker_path(),
            cfg.browser_marker_path(),
            state_path(&cfg.home, &cfg.name, "desktop"),
            state_path(&cfg.home, &cfg.name, "browser"),
        ];
        let mut files = Vec::new();
        for path in paths {
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            files.push((path, bytes));
        }
        Ok(Self(files))
    }

    fn restore(&self) -> Result<()> {
        for (path, bytes) in &self.0 {
            match bytes {
                Some(bytes) => atomic_write_bytes(path, bytes, 0o600)?,
                None => match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                },
            }
        }
        Ok(())
    }
}

fn stop(cfg: &Config, ctx: &StepContext) -> Result<()> {
    let runtime = Runtime::new(cfg);
    if runtime.alive() {
        runtime.stop(&ctx.log_file, Duration::from_secs(60));
    }
    if runtime.alive() {
        return message("The machine could not be stopped; its disk was not changed");
    }
    runtime.cleanup_stale();
    Ok(())
}

fn boot(cfg: &Config, ctx: &StepContext) -> Result<()> {
    let phase = if cfg.device_runtime_marker().exists() {
        Some("runtime")
    } else {
        None
    };
    let runtime = Runtime::new(cfg);
    runtime.start(
        &ctx.log_file,
        Boot {
            phase,
            ..Boot::new(Duration::from_secs(300))
        },
    )?;
    crate::shell::migrate_chrome_autostart(&runtime)
}

fn prepare_running(cfg: &Config, agentd_binary: &Path) -> Result<()> {
    let runtime = Runtime::new(cfg);
    crate::agentd::ensure_helper(&runtime, agentd_binary)?;
    crate::guest_dns::ensure(cfg, &runtime)?;
    crate::workspace::mount_mappings(cfg, &runtime)
}

fn match_guest_environment(cfg: &mut Config, runtime: &Runtime) -> Result<()> {
    if cfg.browser.get("sync_with_system").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }
    let command = format!("{} guest-system environment", crate::agentd::HELPER_BIN);
    let (code, output) = runtime.ssh_capture(&command, Duration::from_secs(20));
    if code != 0 {
        return message(format!(
            "Could not read the machine's language and timezone: {}",
            output.trim()
        ));
    }
    let env: Value = serde_json::from_str(output.trim()).map_err(|error| {
        crate::Error::Message(format!(
            "Could not read the machine's language and timezone: {error}"
        ))
    })?;
    for key in ["locale", "timezone"] {
        let value = env
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                crate::Error::Message(format!("The machine did not report its {key}"))
            })?;
        cfg.browser[key] = json!(value);
    }
    Ok(())
}

fn guest_script(
    runtime: &Runtime,
    ctx: &StepContext,
    label: &str,
    script: &str,
    timeout: Duration,
) -> Result<()> {
    ctx.log(label);
    let (code, output) = runtime.ssh_script(script, timeout);
    if !output.is_empty() {
        crate::util::log_line(&ctx.log_file, &output);
    }
    if code != 0 {
        let tail: String = output
            .chars()
            .rev()
            .take(2500)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return message(format!("{label} failed (exit {code}): {}", tail.trim()));
    }
    Ok(())
}

/// Verify the desktop after boot. The install scripts already verify the Chrome package and profile.
pub fn verify_started(cfg: &Config) -> Result<()> {
    desktop::wait_for_vnc(&Runtime::new(cfg))
}

pub fn can_enable_spice(cfg: &Config) -> bool {
    cfg!(unix)
        && !cfg.desktop_spice
        && !cfg.device_enabled()
        && desktop::enabled(cfg)
        && cfg.desktop_marker_path().is_file()
}

fn restore_native_display(cfg: &Config) -> Result<Option<Value>> {
    if !cfg.desktop_chrome || !cfg.browser_enabled() {
        return Ok(None);
    }
    let native = |request| crate::shell::browser_native_display(&cfg.home, &cfg.name, request);
    let previous = native(json!({"action": "status"}))?;
    let recovery_required = previous["recoveryRequired"].as_bool() == Some(true);
    let enabled = previous["enabled"].as_bool() == Some(true);
    if enabled || recovery_required {
        native(json!({"action": "restore"}))?;
    }
    Ok((enabled && !recovery_required).then(|| {
        json!({"action": "apply", "width": previous["width"], "height": previous["height"],
            "scale": previous["scale"], "zoom": previous["zoom"]})
    }))
}

fn verify_spice(cfg: &Config, runtime: &Runtime) -> Result<()> {
    use std::io::BufRead;
    #[cfg(unix)]
    use std::os::unix::fs::FileTypeExt;

    crate::vm::check_spice_dir(cfg)?;
    let endpoint = cfg.spice_socket();
    #[cfg(unix)]
    if !std::fs::symlink_metadata(&endpoint)?
        .file_type()
        .is_socket()
    {
        return message("QEMU did not open its private SPICE socket");
    }
    let mut qmp = runtime
        .qmp(Duration::from_secs(5))
        .ok_or_else(|| crate::Error::Message("QEMU's display controls did not respond".into()))?;
    let spice = qmp.execute("query-spice", None)?;
    if spice.get("enabled").and_then(Value::as_bool) != Some(true)
        || spice.get("host").and_then(Value::as_str) != endpoint.to_str()
    {
        return message("QEMU did not report a private SPICE display");
    }
    let (width, height) = desktop::drm_output(runtime)?;
    let capture = cfg.spice_dir().join("scanout.ppm");
    let result = (|| -> Result<()> {
        qmp.execute("screendump", Some(json!({"filename": capture})))?;
        let file = std::fs::File::open(&capture)?;
        let size = file.metadata()?.len();
        let mut reader = std::io::BufReader::new(file);
        let mut header = String::new();
        for _ in 0..3 {
            reader.read_line(&mut header)?;
        }
        if header != format!("P6\n{width} {height}\n255\n")
            || size != header.len() as u64 + u64::from(width) * u64::from(height) * 3
        {
            return message("QEMU's scanout did not match the Sway DRM output");
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&capture);
    result
}

/// Move the running desktop to DRM, verify both display transports, then commit
/// the host settings. The guest drop-in falls back to the old compositor mode
/// without a virtio GPU, including if the application exits during conversion.
pub fn enable_spice(old: &Config, agentd_binary: &Path) -> Result<()> {
    if old.desktop_spice {
        return Ok(());
    }
    if !can_enable_spice(old) {
        return message("SPICE requires an installed Sway desktop without the device profile");
    }
    let old_runtime = Runtime::new(old);
    if !old_runtime.running() {
        return message("Start the machine before enabling SPICE");
    }
    let mut cfg = old.clone();
    cfg.gpu_mode = "virtio".into();
    cfg.desktop_spice = true;
    cfg.validate()?;
    let ctx = StepContext::new(old, "desktop-spice", "enable")?;
    let mut desktop_ctx = StepContext::new(&cfg, "desktop", "enable-spice")?;
    let backup = Backup::take(old)?;
    let previous_display = restore_native_display(old)?;
    let runtime = Runtime::new(&cfg);
    let mut restarting = false;
    let result = (|| -> Result<()> {
        desktop::ensure_wayvnc(old, &old_runtime)?;
        guest_script(
            &old_runtime,
            &ctx,
            "Preparing the Sway DRM session",
            &desktop::stage_spice_script(),
            Duration::from_secs(30),
        )?;
        restarting = true;
        stop(old, &ctx)?;
        boot(&cfg, &ctx)?;
        desktop::wait_for_vnc(&runtime)?;
        verify_spice(&cfg, &runtime)?;
        prepare_running(&cfg, agentd_binary)?;
        save_desktop_state(&cfg, &mut desktop_ctx)?;
        cfg.save_instance_meta()
    })();
    let Err(error) = result else { return Ok(()) };
    ctx.log(&format!("SPICE conversion failed: {error}"));
    let rollback = (|| -> Result<()> {
        backup.restore()?;
        if restarting {
            stop(&cfg, &ctx)?;
            boot(old, &ctx)?;
        }
        guest_script(
            &old_runtime,
            &ctx,
            "Restoring the original Sway startup",
            &desktop::unstage_spice_script(),
            Duration::from_secs(30),
        )?;
        if restarting {
            prepare_running(old, agentd_binary)?;
        }
        desktop::wait_for_vnc(&old_runtime)?;
        if let Some(request) = previous_display {
            crate::shell::browser_native_display(&old.home, &old.name, request)?;
        }
        Ok(())
    })();
    match rollback {
        Ok(()) => message(format!(
            "SPICE could not be enabled: {error}. The previous desktop was restored."
        )),
        Err(rollback) => message(format!(
            "SPICE could not be enabled: {error}. Restoring the previous desktop also failed: {rollback}"
        )),
    }
}

fn save_desktop_state(cfg: &Config, ctx: &mut StepContext) -> Result<()> {
    atomic_write_json(
        &cfg.desktop_marker_path(),
        &json!({"installed": true, "name": cfg.name, "desktop": desktop::summary(cfg), "installed_at": now_millis()}),
        0o600,
    )?;
    ctx.track_file(&cfg.desktop_marker_path())?;
    ctx.set_meta("desktop_fingerprint", json!(desktop::fingerprint(cfg)));
    ctx.set_meta("desktop_env", json!(cfg.desktop_env));
    DesktopStep.save_applied(cfg, ctx, "applied", None)?;
    Ok(())
}

fn save_step_states(
    cfg: &Config,
    desktop_ctx: &mut StepContext,
    browser_ctx: &mut StepContext,
    data: &Value,
) -> Result<()> {
    save_desktop_state(cfg, desktop_ctx)?;
    atomic_write_json(
        &cfg.browser_marker_path(),
        &json!({"installed": true, "name": cfg.name, "browser": browser::summary(cfg, Some(data))?, "installed_at": now_millis()}),
        0o600,
    )?;
    browser_ctx.track_file(&cfg.browser_marker_path())?;
    browser_ctx.set_meta("browser_fingerprint", json!(browser::fingerprint(cfg)?));
    BrowserStep.save_applied(cfg, browser_ctx, "applied", None)?;
    cfg.save_instance_meta()?;
    Ok(())
}

/// Keep the machine's existing graphics and device settings; the desktop is
/// shown through the guest's loopback VNC server over SSH.
pub fn install(old: &Config, agentd_binary: &Path) -> Result<()> {
    let mut cfg = old.clone();
    cfg.desktop_env = Some("sway".into());
    cfg.desktop_chrome = true;
    cfg.browser["enabled"] = json!(true);
    cfg.validate()?;
    let ctx = StepContext::new(old, "chrome", "install")?;
    let backup = Backup::take(old)?;
    let was_running = Runtime::new(old).running();
    let mut desktop_ctx = StepContext::new(&cfg, "desktop", "chrome-install")?;
    let mut browser_ctx = StepContext::new(&cfg, "browser", "chrome-install")?;
    desktop_ctx.agentd_binary = Some(agentd_binary.to_path_buf());
    browser_ctx.agentd_binary = Some(agentd_binary.to_path_buf());
    let result = (|| -> Result<()> {
        stop(old, &ctx)?;
        boot(&cfg, &ctx)?;
        let runtime = Runtime::new(&cfg);
        crate::agentd::ensure_helper(&runtime, agentd_binary)?;
        crate::guest_dns::ensure(&cfg, &runtime)?;
        guest_script(
            &runtime,
            &ctx,
            "Installing the desktop and Google Chrome",
            &desktop::install_script(&cfg)?,
            Duration::from_secs(1800),
        )?;
        guest_script(
            &runtime,
            &ctx,
            "Checking the desktop and Chrome package",
            &desktop::verify_script(&cfg),
            Duration::from_secs(120),
        )?;
        // The desktop session starts on the next boot.
        stop(&cfg, &ctx)?;
        boot(&cfg, &ctx)?;
        crate::agentd::ensure_helper(&runtime, agentd_binary)?;
        match_guest_environment(&mut cfg, &runtime)?;
        let data = browser::resolve(&cfg)?;
        let profile = browser::install_profile(&data);
        guest_script(
            &runtime,
            &ctx,
            "Configuring the browser profile",
            &browser::install_script_preserving_system(&cfg, &profile)?,
            Duration::from_secs(600),
        )?;
        guest_script(
            &runtime,
            &ctx,
            "Checking the browser profile",
            &browser::verify_script_preserving_system(&cfg, &profile)?,
            Duration::from_secs(120),
        )?;
        stop(&cfg, &ctx)?;
        boot(&cfg, &ctx)?;
        crate::agentd::ensure_helper(&runtime, agentd_binary)?;
        desktop::wait_for_vnc(&runtime)?;
        prepare_running(&cfg, agentd_binary)?;
        save_step_states(&cfg, &mut desktop_ctx, &mut browser_ctx, &data)
    })();
    let Err(error) = result else {
        return Ok(());
    };
    ctx.log(&format!("Chrome setup failed: {error}"));
    match put_back(old, &cfg, &ctx, &backup, was_running, agentd_binary) {
        Ok(()) => message(format!(
            "Chrome setup failed: {error}. The machine's settings were put back; you can try again."
        )),
        Err(cleanup) => message(format!(
            "Chrome setup failed: {error}. Putting the machine's settings back also failed: {cleanup}"
        )),
    }
}

/// Apply changed browser settings to the running machine without a
/// reinstall or reboot: write the new profile in place, check it, record the
/// browser step and restart Chrome so it starts with them. On failure the
/// host records are restored and the previous profile is written back.
pub fn apply_profile(old: &Config, updated: &Config, agentd_binary: &Path) -> Result<()> {
    let runtime = Runtime::new(old);
    if !runtime.running() {
        return message("Start the machine to change the browser settings");
    }
    if !old.browser_enabled() || !old.browser_marker_path().exists() {
        return message("Set up Chrome before changing its settings");
    }
    updated.validate()?;
    let data = browser::resolve(updated)?;
    browser::validate(&data, updated)?;
    let ctx = StepContext::new(old, "browser", "settings")?;
    let backup = Backup::take(old)?;
    let mut browser_ctx = StepContext::new(updated, "browser", "settings")?;
    browser_ctx.agentd_binary = Some(agentd_binary.to_path_buf());
    let result = (|| -> Result<()> {
        crate::agentd::ensure_helper(&runtime, agentd_binary)?;
        let profile = browser::install_profile(&data);
        guest_script(
            &runtime,
            &ctx,
            "Writing the browser profile",
            &browser::install_script_preserving_system(updated, &profile)?,
            Duration::from_secs(600),
        )?;
        guest_script(
            &runtime,
            &ctx,
            "Checking the browser profile",
            &browser::verify_script_preserving_system(updated, &profile)?,
            Duration::from_secs(120),
        )?;
        atomic_write_json(
            &updated.browser_marker_path(),
            &json!({"installed": true, "name": updated.name, "browser": browser::summary(updated, Some(&data))?, "installed_at": now_millis()}),
            0o600,
        )?;
        browser_ctx.track_file(&updated.browser_marker_path())?;
        browser_ctx.set_meta("browser_fingerprint", json!(browser::fingerprint(updated)?));
        BrowserStep.save_applied(updated, &browser_ctx, "applied", None)?;
        updated.save_instance_meta()?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            if crate::shell::browser_status(&old.home, &old.name)?["chrome_running"] == true {
                let result = crate::shell::browser_stop(&updated.home, &updated.name)?;
                if result.code != 0 {
                    return message(result.output);
                }
                let result =
                    crate::shell::browser_open(&updated.home, &updated.name, "about:blank")?;
                if result.code != 0 {
                    return message(result.output);
                }
            }
            Ok(())
        }
        Err(error) => {
            ctx.log(&format!("Browser settings failed: {error}"));
            let restored = backup.restore().and_then(|()| {
                let profile = browser::install_profile(&browser::resolve(old)?);
                guest_script(
                    &runtime,
                    &ctx,
                    "Writing the previous browser profile back",
                    &browser::install_script_preserving_system(old, &profile)?,
                    Duration::from_secs(600),
                )
            });
            match restored {
                Ok(()) => message(format!(
                    "The browser settings were not applied: {error}. The previous settings are back."
                )),
                Err(cleanup) => message(format!(
                    "The browser settings were not applied: {error}. Putting the previous settings back also failed: {cleanup}"
                )),
            }
        }
    }
}

/// After a failed setup: switch a desktop the machine did not have back off
/// in the guest (best effort), restore the host records, restart if needed.
fn put_back(
    old: &Config,
    cfg: &Config,
    ctx: &StepContext,
    backup: &Backup,
    was_running: bool,
    agentd_binary: &Path,
) -> Result<()> {
    let runtime = Runtime::new(cfg);
    if old.desktop_env.is_none() && runtime.alive() {
        let (_, output) = runtime.ssh_capture(
            "sudo -n systemctl disable sway-autologin.service 2>/dev/null; sudo -n systemctl set-default multi-user.target",
            Duration::from_secs(30),
        );
        ctx.log(&output);
    }
    stop(cfg, ctx)?;
    backup.restore()?;
    if was_running {
        let old = crate::config::load_instance_config(&old.project_root, &old.home, &old.name)?;
        boot(&old, ctx)?;
        prepare_running(&old, agentd_binary)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_chrome_install_log_is_selected() {
        let home = std::env::temp_dir().join(format!(
            "cabin-chrome-log-{}-{}",
            std::process::id(),
            crate::util::localstamp()
        ));
        let cfg = Config {
            name: "agent".into(),
            home: home.clone(),
            ..Config::default()
        };
        let dir = cfg.instance_logs_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("chrome-install-20260101-000000.log"), "old").unwrap();
        std::fs::write(dir.join("chrome-install-20261118-120000.log"), "new").unwrap();
        std::fs::write(dir.join("chrome-install-notes.txt"), "not a log").unwrap();

        assert_eq!(
            install_log_path(&cfg),
            Some(dir.join("chrome-install-20261118-120000.log"))
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
