//! Native host workspace actions and atomic host-to-guest project sync.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{Config, default_project_root, load_instance_config};
use crate::lock::FileLock;
use crate::paths::{InstancePaths, StatePaths};
use crate::util::{atomic_write_json, ensure_dir, read_json, shell_quote};
use crate::vm::Runtime;
use crate::{Error, Result};

/// Live host-folder mappings are stored separately from the legacy copy-based
/// workspace so upgrading never changes the meaning of an existing project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mapping {
    pub host_path: String,
    pub guest_path: String,
    pub tag: String,
}

fn mappings_file(cfg: &Config) -> PathBuf {
    InstancePaths::new(cfg.instance_dir()).mappings()
}

fn save_mappings(cfg: &Config, items: &[Mapping]) -> Result<()> {
    ensure_dir(&cfg.instance_dir())?;
    atomic_write_json(&mappings_file(cfg), &serde_json::to_value(items)?, 0o600)
}

pub fn mappings(cfg: &Config) -> Result<Vec<Mapping>> {
    let path = mappings_file(cfg);
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn valid_guest_path(path: &str) -> bool {
    let path = Path::new(path);
    path.is_absolute()
        && path != Path::new("/")
        && path.components().all(|part| {
            matches!(
                part,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
}

fn default_guest_path(items: &[Mapping], host_path: &Path) -> String {
    let preferred = ["/root/P0", "/root/P1", "/root/P2"]
        .into_iter()
        .find(|candidate| {
            !items.iter().any(|mapping| {
                Path::new(candidate).starts_with(&mapping.guest_path)
                    || Path::new(&mapping.guest_path).starts_with(candidate)
            })
        });
    preferred.map(str::to_string).unwrap_or_else(|| {
        format!(
            "/root/{}",
            host_path.file_name().unwrap_or_default().to_string_lossy()
        )
    })
}

pub fn save_mapping(
    home: &Path,
    name: &str,
    host_path: &str,
    guest_path: Option<&str>,
) -> Result<Vec<Mapping>> {
    let _lock = FileLock::acquire(&StatePaths::new(home).operation_lock())?;
    let cfg = config_for(home, name)?;
    if Runtime::new(&cfg).running() {
        return Err(Error::Message(
            "Stop the instance before changing mappings".into(),
        ));
    }
    let host = expand_home(host_path.trim()).canonicalize()?;
    if !host.is_dir() || host.parent().is_none() {
        return Err(Error::Message("Choose a host folder".into()));
    }
    let host = host
        .to_str()
        .ok_or_else(|| Error::Message("Folder path is not UTF-8".into()))?;
    let mut items = mappings(&cfg)?;
    let default = default_guest_path(&items, Path::new(host));
    let guest = guest_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(&default);
    if !valid_guest_path(guest) {
        return Err(Error::Message(
            "Guest path must be an absolute folder path".into(),
        ));
    }
    if items.len() >= 16 {
        return Err(Error::Message("At most 16 folders can be mapped".into()));
    }
    if items.iter().any(|m| {
        m.host_path == host
            || Path::new(&m.guest_path).starts_with(guest)
            || Path::new(guest).starts_with(&m.guest_path)
    }) {
        return Err(Error::Message(
            "Folder or guest path is already mapped".into(),
        ));
    }
    let tag = format!("vmctl{}", &crate::util::sha256_text(host)[..16]);
    items.push(Mapping {
        host_path: host.into(),
        guest_path: guest.into(),
        tag,
    });
    save_mappings(&cfg, &items)?;
    Ok(items)
}

pub fn remove_mapping(home: &Path, name: &str, tag: &str) -> Result<Vec<Mapping>> {
    let _lock = FileLock::acquire(&StatePaths::new(home).operation_lock())?;
    let cfg = config_for(home, name)?;
    if Runtime::new(&cfg).running() {
        return Err(Error::Message(
            "Stop the instance before changing mappings".into(),
        ));
    }
    let mut items = mappings(&cfg)?;
    items.retain(|m| m.tag != tag);
    save_mappings(&cfg, &items)?;
    Ok(items)
}

/// QEMU key-value arguments escape literal commas by doubling them.
pub fn qemu_args(cfg: &Config) -> Result<Vec<String>> {
    let mut args = Vec::new();
    for mapping in mappings(cfg)? {
        if !Path::new(&mapping.host_path).is_dir()
            || !valid_guest_path(&mapping.guest_path)
            || !mapping.tag.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(Error::Message(format!(
                "Invalid mapping: {}",
                mapping.host_path
            )));
        }
        args.extend([
            "-fsdev".into(),
            format!(
                "local,id={},path={},security_model=none",
                mapping.tag,
                mapping.host_path.replace(',', ",,")
            ),
            "-device".into(),
            format!(
                "virtio-9p-pci,fsdev={},mount_tag={}",
                mapping.tag, mapping.tag
            ),
        ]);
    }
    Ok(args)
}

pub fn mount_mappings(cfg: &Config, runtime: &Runtime) -> Result<()> {
    for mapping in mappings(cfg)? {
        let target = shell_quote(&mapping.guest_path);
        let tag = shell_quote(&mapping.tag);
        // Refuse to hide existing guest files under a mount point.
        let script = format!(
            "set -eu; p={target}; t={tag}; if mountpoint -q \"$p\"; then [ \"$(findmnt -n -o SOURCE --target \"$p\")\" = \"$t\" ]; exit; fi; if [ -e \"$p\" ] && {{ [ ! -d \"$p\" ] || [ -n \"$(ls -A \"$p\")\" ]; }}; then echo \"Mount path is not empty: $p\" >&2; exit 1; fi; mkdir -p \"$p\"; mount -t 9p -o trans=virtio,version=9p2000.L \"$t\" \"$p\""
        );
        let command = format!("sudo -n sh -c {}", shell_quote(&script));
        let (code, output) = runtime.ssh_capture(&command, Duration::from_secs(30));
        if code != 0 {
            return Err(Error::Message(format!(
                "Could not mount {}: {}",
                mapping.guest_path,
                output.trim()
            )));
        }
    }
    Ok(())
}

fn config_for(home: &Path, name: &str) -> Result<Config> {
    load_instance_config(&default_project_root(), home, name)
}

fn workspace_file(cfg: &Config) -> PathBuf {
    InstancePaths::new(cfg.instance_dir()).workspace()
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

fn selected_path(cfg: &Config) -> Option<PathBuf> {
    read_json(&workspace_file(cfg)).and_then(|value| {
        value
            .get("host_path")
            .and_then(Value::as_str)
            .map(expand_home)
    })
}

fn guest_path(cfg: &Config, host_path: &Path) -> Option<String> {
    host_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|base| format!("/home/{}/workspace/{base}", cfg.current_username()))
}

pub fn status(home: &Path, name: &str) -> Result<Value> {
    let cfg = config_for(home, name)?;
    let host_path = selected_path(&cfg);
    let guest_path = host_path.as_deref().and_then(|path| guest_path(&cfg, path));
    Ok(json!({
        "name": cfg.name,
        "configured": host_path.is_some(),
        "host_path": host_path.as_ref().map(|path| path.display().to_string()),
        "host_exists": host_path.as_ref().is_some_and(|path| path.is_dir()),
        "guest_path": guest_path,
        "guest_user": cfg.current_username(),
        "vm_running": Runtime::new(&cfg).running(),
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn guest_paths_are_absolute_and_cannot_traverse_parents() {
        assert!(super::valid_guest_path("/root/project with spaces"));
        assert!(!super::valid_guest_path("/"));
        assert!(!super::valid_guest_path("relative/project"));
        assert!(!super::valid_guest_path("/root/../etc"));
    }

    #[test]
    fn default_guest_paths_fill_p0_p1_and_p2_before_using_the_folder_name() {
        let host = std::path::Path::new("/tmp/project");
        let mapping = |guest_path: &str| super::Mapping {
            host_path: format!("/tmp/{guest_path}"),
            guest_path: guest_path.into(),
            tag: "vmctl123".into(),
        };

        assert_eq!(super::default_guest_path(&[], host), "/root/P0");
        assert_eq!(
            super::default_guest_path(&[mapping("/root/P0")], host),
            "/root/P1"
        );
        assert_eq!(
            super::default_guest_path(&[mapping("/root/P0"), mapping("/root/P1")], host,),
            "/root/P2"
        );
        assert_eq!(
            super::default_guest_path(
                &[
                    mapping("/root/P0"),
                    mapping("/root/P1"),
                    mapping("/root/P2"),
                ],
                host,
            ),
            "/root/project"
        );
    }

    #[test]
    fn qemu_mapping_escapes_commas_and_rejects_invalid_tags() {
        let root = fixture("mapping,comma");
        let cfg = crate::config::Config {
            home: root.clone(),
            name: "test".into(),
            ..crate::config::Config::default()
        };
        std::fs::create_dir_all(cfg.instance_dir()).unwrap();
        let mapping = super::Mapping {
            host_path: root.display().to_string(),
            guest_path: "/root/project".into(),
            tag: "vmctl123".into(),
        };
        std::fs::write(
            super::mappings_file(&cfg),
            serde_json::to_vec(&vec![mapping.clone()]).unwrap(),
        )
        .unwrap();
        let args = super::qemu_args(&cfg).unwrap();
        assert!(args[1].contains("mapping,,comma"));
        assert!(args[3].contains("mount_tag=vmctl123"));
        let invalid = super::Mapping {
            tag: "bad,tag".into(),
            ..mapping
        };
        std::fs::write(
            super::mappings_file(&cfg),
            serde_json::to_vec(&vec![invalid]).unwrap(),
        )
        .unwrap();
        assert!(super::qemu_args(&cfg).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mapping_registry_write_failure_preserves_the_previous_file() {
        let root = fixture("mapping-write-failure");
        let cfg = crate::config::Config {
            home: root.clone(),
            name: "test".into(),
            ..crate::config::Config::default()
        };
        let instance = cfg.instance_dir();
        fs::create_dir_all(&instance).unwrap();
        let previous = super::Mapping {
            host_path: root.display().to_string(),
            guest_path: "/root/previous".into(),
            tag: "vmctlprevious".into(),
        };
        super::save_mappings(&cfg, std::slice::from_ref(&previous)).unwrap();
        let before = fs::read(super::mappings_file(&cfg)).unwrap();
        fs::set_permissions(&instance, fs::Permissions::from_mode(0o500)).unwrap();
        let next = super::Mapping {
            host_path: root.display().to_string(),
            guest_path: "/root/next".into(),
            tag: "vmctlnext".into(),
        };
        assert!(super::save_mappings(&cfg, &[next]).is_err());
        assert_eq!(fs::read(super::mappings_file(&cfg)).unwrap(), before);
        fs::set_permissions(&instance, fs::Permissions::from_mode(0o700)).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mapping_changes_respect_the_instance_operation_lock() {
        let root = fixture("mapping-lock");
        let cfg = crate::config::build_config(
            &serde_json::json!({"name": "test", "home": root}),
            &crate::config::default_project_root(),
        )
        .unwrap();
        fs::create_dir_all(cfg.instance_dir()).unwrap();
        cfg.save_instance_meta().unwrap();
        let first_host = root.join("stage/project");
        let second_host = root.join("workspace/project");
        let first = super::save_mapping(&root, "test", first_host.to_str().unwrap(), None).unwrap();
        let lock_path = crate::paths::StatePaths::new(&root).operation_lock();
        let lock = crate::lock::FileLock::acquire(&lock_path).unwrap();

        let error =
            super::save_mapping(&root, "test", second_host.to_str().unwrap(), None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("another vmctl operation is in progress")
        );
        assert!(super::remove_mapping(&root, "test", &first[0].tag).is_err());
        assert_eq!(super::mappings(&cfg).unwrap(), first);

        drop(lock);
        let both = super::save_mapping(&root, "test", second_host.to_str().unwrap(), None).unwrap();
        assert_eq!(both.len(), 2);
        let remaining = super::remove_mapping(&root, "test", &first[0].tag).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].guest_path, "/root/P1");
        let _ = fs::remove_dir_all(root);
    }

    fn fixture(label: &str) -> std::path::PathBuf {
        let unique = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(format!("vmctl-workspace-{label}-{unique}"));
        fs::create_dir_all(root.join("stage/project")).unwrap();
        fs::create_dir_all(root.join("workspace/project")).unwrap();
        fs::write(root.join("stage/project/new.txt"), "new").unwrap();
        fs::write(root.join("workspace/project/old.txt"), "old").unwrap();
        root
    }
}
