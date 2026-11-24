//! Pending resources are kept separately from the config used by a running VM.
//! The request survives both host disk growth and guest filesystem verification.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::util::{atomic_write_json, capture_env, format_bytes, message, parse_size_bytes};
use crate::{Config, Error, Result};

const GIB: u64 = 1024 * 1024 * 1024;
const MAX_DISK_GIB: u32 = 2048;
const REQUEST: &str = "pending-resources.json";
const NOTICE: &str = "disk-growth-notice.txt";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ResourceRequest {
    pub cpus: u32,
    pub memory_mb: u32,
    // Total capacity, not the amount to add.
    pub disk_gib: Option<u32>,
}

pub(crate) fn pending(cfg: &Config) -> Result<Option<ResourceRequest>> {
    match std::fs::read(cfg.instance_file(REQUEST)) {
        Ok(contents) => Ok(Some(serde_json::from_slice(&contents)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn clear(cfg: &Config) -> Result<()> {
    match std::fs::remove_file(cfg.instance_file(REQUEST)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn desired_config(cfg: &Config, request: ResourceRequest) -> Result<Config> {
    let mut next = cfg.clone();
    next.cpus = request.cpus;
    next.memory_mb = request.memory_mb;
    if let Some(gib) = request.disk_gib {
        if !(1..=MAX_DISK_GIB).contains(&gib) {
            return message(format!(
                "Disk capacity must be between 1 and {MAX_DISK_GIB} GiB"
            ));
        }
        let target = u64::from(gib) * GIB;
        let old = parse_size_bytes(&cfg.disk_size)?;
        if target < old {
            return message("The machine disk can only grow; shrinking is not allowed");
        }
        if target > old {
            next.disk_size = format!("{gib}G");
        }
    }
    next.validate()?;
    if next.memory_mb > cfg.memory_mb {
        let needed = u64::from(next.memory_mb) + crate::readiness::HOST_HEADROOM_MB;
        if crate::readiness::physical_memory_mb().is_some_and(|host| needed > host) {
            return message("Leave at least 2 GiB of memory for the host");
        }
    }
    Ok(next)
}

fn preserve_growth(request: ResourceRequest, queued: Option<u32>) -> Result<ResourceRequest> {
    if matches!((request.disk_gib, queued), (Some(requested), Some(queued)) if requested < queued) {
        return message(
            "The pending disk capacity cannot be reduced; wait for the expansion to finish",
        );
    }
    Ok(ResourceRequest {
        disk_gib: request.disk_gib.or(queued),
        ..request
    })
}

pub(crate) fn save(cfg: &Config, request: ResourceRequest) -> Result<Option<ResourceRequest>> {
    check_paths(cfg)?;
    if !cfg.provision_marker_path().is_file() || !cfg.disk_path().is_file() {
        return message("Finish installing the machine before changing its resources");
    }
    let queued = pending(cfg)?.and_then(|previous| previous.disk_gib);
    let request = preserve_growth(request, queued)?;
    let next = desired_config(cfg, request)?;
    let target = parse_size_bytes(&next.disk_size)?;
    let current = parse_size_bytes(&cfg.disk_size)?;
    if request.disk_gib.is_some() && request.disk_gib != queued {
        if queued.is_some_and(|gib| u64::from(gib) * GIB <= current) {
            return message(
                "Finish the current guest filesystem expansion before requesting a larger disk",
            );
        }
        let actual =
            crate::steps::base::disk_virtual_size(cfg, &cfg.disk_path()).ok_or_else(|| {
                Error::Message("Could not read the machine disk's virtual size".into())
            })?;
        if target < actual {
            return message(
                "The requested disk is smaller than the existing image; shrinking is not allowed",
            );
        }
        if target > actual {
            check_free_space(cfg, target - actual)?;
        }
    }
    let request = ResourceRequest {
        disk_gib: request
            .disk_gib
            .filter(|_| target > current || queued.is_some()),
        ..request
    };
    if request.cpus == cfg.cpus && request.memory_mb == cfg.memory_mb && request.disk_gib.is_none()
    {
        clear(cfg)?;
        return Ok(None);
    }
    atomic_write_json(&cfg.instance_file(REQUEST), &json!(request), 0o600)?;
    Ok(Some(request))
}

/// Only an unapplied request may be cancelled. An already-expanded image is
/// never resized back, even if the previous boot was interrupted.
pub(crate) fn cancel_disk(cfg: &Config) -> Result<Option<ResourceRequest>> {
    check_paths(cfg)?;
    let Some(request) = pending(cfg)? else {
        return Ok(None);
    };
    let Some(gib) = request.disk_gib else {
        return Ok(Some(request));
    };
    let current = parse_size_bytes(&cfg.disk_size)?;
    let image = read_image(
        &std::path::absolute(crate::vm::resolve_qemu_img(cfg)?)?,
        &cfg.instance_dir(),
    )?;
    if u64::from(gib) * GIB <= current || image["virtual-size"].as_u64() != Some(current) {
        return message(
            "The disk image has already changed; start the machine to finish expanding the guest filesystem",
        );
    }
    if request.cpus == cfg.cpus && request.memory_mb == cfg.memory_mb {
        clear(cfg)?;
        return Ok(None);
    }
    let request = ResourceRequest {
        disk_gib: None,
        ..request
    };
    atomic_write_json(&cfg.instance_file(REQUEST), &json!(request), 0o600)?;
    Ok(Some(request))
}

pub(crate) fn notice(cfg: &Config) -> Option<String> {
    std::fs::read_to_string(cfg.instance_file(NOTICE)).ok()
}

pub(crate) fn record_notice(cfg: &Config, error: &str) -> Result<()> {
    crate::util::atomic_write_text(&cfg.instance_file(NOTICE), error, 0o600)
}

/// Called only after SSH is ready, under the same operation lock as image growth.
/// Leave the request in place on error so the next cold boot can retry safely.
pub(crate) fn verify_boot(cfg: &Config, runtime: &crate::vm::Runtime) -> Result<()> {
    let Some(request) = pending(cfg)? else {
        return Ok(());
    };
    if request.cpus != cfg.cpus || request.memory_mb != cfg.memory_mb {
        return Ok(());
    }
    if let Some(gib) = request.disk_gib {
        let target = u64::from(gib) * GIB;
        if parse_size_bytes(&cfg.disk_size)? != target {
            return Ok(());
        }
        crate::agentd::grow_root(runtime, target)?;
    }
    match std::fs::remove_file(cfg.instance_file(NOTICE)) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    clear(cfg)
}

fn check_paths(cfg: &Config) -> Result<()> {
    use std::io::ErrorKind::NotFound;
    for path in [
        cfg.home.clone(),
        crate::paths::StatePaths::new(&cfg.home).instances(),
        cfg.instance_dir(),
        cfg.disk_path(),
        cfg.instance_meta_path(),
        cfg.instance_file(REQUEST),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return message(format!(
                    "Refusing to change resources through a symlink: {}",
                    path.display()
                ));
            }
            Ok(_) => (),
            Err(error) if error.kind() == NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn check_free_space(cfg: &Config, growth: u64) -> Result<()> {
    let free = fs4::available_space(cfg.instance_dir())?;
    let reserve = crate::readiness::IMAGE_HEADROOM_GB;
    if growth.saturating_add(reserve * GIB) > free {
        return message(format!(
            "Not enough host disk space to add {} and leave {reserve} GiB free ({} available)",
            format_bytes(Some(growth)),
            format_bytes(Some(free))
        ));
    }
    Ok(())
}

/// Called under the home operation lock, before the normal installation checks.
/// Write the applied config only after qemu-img verifies the new virtual size.
pub(crate) fn apply_stopped(cfg: Config, request: ResourceRequest) -> Result<Config> {
    check_paths(&cfg)?;
    if !cfg.provision_marker_path().is_file() || !cfg.disk_path().is_file() {
        return message("Finish installing the machine before changing its resources");
    }
    let next = desired_config(&cfg, request)?;
    if request.disk_gib.is_some() {
        grow_image(&cfg, parse_size_bytes(&next.disk_size)?)?;
    }
    if cfg.cpus != next.cpus || cfg.memory_mb != next.memory_mb || cfg.disk_size != next.disk_size {
        next.save_instance_meta()?;
    }
    Ok(next)
}

fn image_command(tool: &Path, dir: &Path, args: &[&str]) -> Result<String> {
    let mut argv = vec![tool.display().to_string()];
    argv.extend(args.iter().map(|arg| (*arg).to_string()));
    let output = capture_env(&argv, Duration::from_secs(120), None, Some(dir), None);
    if output.code != 0 {
        return message(format!(
            "qemu-img {} failed (exit {}): {}",
            args[0],
            output.code,
            output.output.trim()
        ));
    }
    Ok(output.output)
}

fn image_capacity(info: &Value) -> Result<u64> {
    if info["format"].as_str() != Some("qcow2")
        || !info["backing-filename"].is_null()
        || info.pointer("/format-specific/data/data-file").is_some()
    {
        return message("Disk expansion requires Cabin's standalone qcow2 image");
    }
    if info["snapshots"]
        .as_array()
        .is_some_and(|items| !items.is_empty())
    {
        return message(
            "The machine disk still has legacy snapshots. Stop the machine normally before expanding the disk",
        );
    }
    info["virtual-size"]
        .as_u64()
        .filter(|size| *size > 0)
        .ok_or_else(|| Error::Message("Could not read the machine disk's virtual size".into()))
}

fn read_image(tool: &Path, dir: &Path) -> Result<Value> {
    // Relative file operands keep Windows user-directory Unicode out of QEMU argv.
    // Do not use -U here: another process holding the disk must block the resize.
    let info = image_command(tool, dir, &["info", "--output=json", "disk.qcow2"])?;
    Ok(serde_json::from_str(&info)?)
}

fn capacity(tool: &Path, dir: &Path) -> Result<u64> {
    image_capacity(&read_image(tool, dir)?)
}

fn grow_image(cfg: &Config, target: u64) -> Result<()> {
    grow_image_with(
        &std::path::absolute(crate::vm::resolve_qemu_img(cfg)?)?,
        &cfg.instance_dir(),
        target,
        |growth| check_free_space(cfg, growth),
    )
}

fn grow_image_with(
    tool: &Path,
    dir: &Path,
    target: u64,
    check_space: impl FnOnce(u64) -> Result<()>,
) -> Result<()> {
    let current = capacity(tool, dir)?;
    let Some(growth) = target.checked_sub(current) else {
        return message(
            "The requested disk is smaller than the existing image; shrinking is not allowed",
        );
    };
    if growth != 0 {
        check_space(growth)?;
        // QEMU also rejects shrinking unless explicitly passed --shrink.
        image_command(
            tool,
            dir,
            &["resize", "-f", "qcow2", "disk.qcow2", &target.to_string()],
        )?;
        if capacity(tool, dir)? != target {
            return message("QEMU did not report the expected expanded capacity");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smaller_existing_or_queued_sizes_are_rejected() {
        let cfg = Config::default();
        let request = ResourceRequest {
            cpus: cfg.cpus,
            memory_mb: cfg.memory_mb,
            disk_gib: Some(19),
        };
        assert!(
            desired_config(&cfg, request)
                .unwrap_err()
                .to_string()
                .contains("shrinking")
        );
        assert!(preserve_growth(request, Some(40)).is_err());
        assert_eq!(
            preserve_growth(
                ResourceRequest {
                    disk_gib: None,
                    ..request
                },
                Some(40)
            )
            .unwrap()
            .disk_gib,
            Some(40)
        );
    }

    #[test]
    fn saved_compute_allocation_keeps_live_config_and_disk_unchanged_until_apply() {
        let path = Scratch::new("cabin-compute-allocation");
        let cfg = crate::config::build_config(
            &json!({"name": "resource-test", "home": path.0}),
            &crate::paths::default_project_root(),
        )
        .unwrap();
        std::fs::create_dir_all(cfg.instance_dir()).unwrap();
        std::fs::write(cfg.provision_marker_path(), b"{}").unwrap();
        std::fs::write(cfg.disk_path(), b"disk still here").unwrap();
        cfg.save_instance_meta().unwrap();
        let next = ResourceRequest {
            cpus: cfg.cpus + 1,
            memory_mb: cfg.memory_mb - 512,
            disk_gib: None,
        };
        assert_eq!(save(&cfg, next).unwrap(), Some(next));
        assert_eq!(pending(&cfg).unwrap(), Some(next));
        let applied = || {
            crate::config::load_instance_config(&cfg.project_root, &cfg.home, &cfg.name).unwrap()
        };
        assert_eq!(applied().cpus, cfg.cpus);
        assert_eq!(applied().memory_mb, cfg.memory_mb);
        apply_stopped(cfg.clone(), next).unwrap();
        assert_eq!(applied().cpus, next.cpus);
        assert_eq!(applied().memory_mb, next.memory_mb);
        assert_eq!(std::fs::read(cfg.disk_path()).unwrap(), b"disk still here");
    }

    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn rejects_backed_images_and_snapshots() {
        for info in [
            json!({"format": "raw", "virtual-size": 1024}),
            json!({"format": "qcow2", "virtual-size": 1024, "backing-filename": "base.qcow2"}),
            json!({"format": "qcow2", "virtual-size": 1024, "snapshots": [{"name": "legacy"}]}),
            json!({"format": "qcow2", "virtual-size": 0}),
        ] {
            assert!(image_capacity(&info).is_err());
        }
    }

    #[test]
    #[ignore = "uses qemu-img and qemu-io on a disposable qcow2"]
    fn grows_existing_qcow2_without_touching_contents_and_is_idempotent() {
        let path = Scratch::new("cabin-disk-growth");
        let tool = which::which("qemu-img").unwrap();
        let io = which::which("qemu-io").unwrap();
        image_command(
            &tool,
            &path.0,
            &["create", "-f", "qcow2", "disk.qcow2", "32M"],
        )
        .unwrap();
        image_command(&io, &path.0, &["-c", "write -P 0x5a 0 4k", "disk.qcow2"]).unwrap();
        let target = 64 * 1024 * 1024;
        grow_image_with(&tool, &path.0, target, |growth| {
            assert_eq!(growth, 32 * 1024 * 1024);
            Ok(())
        })
        .unwrap();
        grow_image_with(&tool, &path.0, target, |_| panic!("already expanded")).unwrap();
        assert!(
            grow_image_with(&tool, &path.0, target - 512, |_| panic!("must not shrink")).is_err()
        );
        assert_eq!(capacity(&tool, &path.0).unwrap(), target);
        let output =
            image_command(&io, &path.0, &["-c", "read -P 0x5a 0 4k", "disk.qcow2"]).unwrap();
        assert!(!output.contains("Pattern verification failed"), "{output}");
    }
}
