//! Instance disk step: full copy of the base image, resize,
//! UEFI variable store.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::Result;
use crate::steps::base::{
    CheckResult, Step, StepContext, delete_artifacts, disk_info, disk_virtual_size, ensure_stopped,
};
use crate::util::{
    CaptureResult, capture_env, chmod, ensure_dir, format_bytes, message, parse_size_bytes,
    read_json,
};

fn atomic_copy(src: &Path, dest: &Path, mode: u32) -> Result<()> {
    if let Some(parent) = dest.parent() {
        ensure_dir(parent)?;
    }
    let tmp = dest.with_file_name(format!(
        ".{}.tmp",
        dest.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("file")
    ));
    let result = (|| -> Result<()> {
        std::fs::copy(src, &tmp)?;
        chmod(&tmp, mode)?;
        std::fs::rename(&tmp, dest)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn check_free_space(path: &Path, needed: u64) -> Result<()> {
    let free = fs4::available_space(path).map_err(|error| {
        crate::Error::Message(format!(
            "could not check free disk space at {}: {error}",
            path.display()
        ))
    })?;
    if free < needed {
        return message(format!(
            "not enough free disk space at {}: {} available, {} needed for the VM disk",
            path.display(),
            format_bytes(Some(free)),
            format_bytes(Some(needed))
        ));
    }
    Ok(())
}

fn create_disk(
    home: &Path,
    qemu_img: &Path,
    base: &Path,
    disk: &Path,
    wanted: u64,
    mut run: impl FnMut(&[String], Duration, &Path) -> CaptureResult,
) -> Result<()> {
    // qemu-img on Windows uses narrow argv. The Unicode user profile is
    // carried through the working directory, with relative file operands.
    let relative = |path: &Path| {
        path.strip_prefix(home)
            .map(|path| path.display().to_string())
            .map_err(|_| {
                crate::Error::Message(format!(
                    "qemu-img file is outside the Cabin data directory: {}",
                    path.display()
                ))
            })
    };
    let base = relative(base)?;
    let disk = relative(disk)?;
    let qemu_img = std::path::absolute(qemu_img)?.display().to_string();
    for (args, timeout) in [
        (
            vec![
                qemu_img.clone(),
                "convert".into(),
                "-O".into(),
                "qcow2".into(),
                base,
                disk.clone(),
            ],
            Duration::from_secs(900),
        ),
        (
            vec![qemu_img, "resize".into(), disk, wanted.to_string()],
            Duration::from_secs(300),
        ),
    ] {
        let result = run(&args, timeout, home);
        if result.code == 124 {
            return message(format!(
                "qemu-img {} timed out while preparing the VM disk",
                args[1]
            ));
        }
        if result.code != 0 {
            let detail = result.output.trim();
            let lower = detail.to_ascii_lowercase();
            if lower.contains("no space left on device")
                || lower.contains("not enough space on the disk")
            {
                return message(format!(
                    "not enough free disk space to create the VM disk: {detail}"
                ));
            }
            return message(format!(
                "qemu-img {} failed (exit {}): {detail}",
                args[1], result.code
            ));
        }
    }
    Ok(())
}

pub struct DiskStep;

impl Step for DiskStep {
    fn id(&self) -> &str {
        "disk"
    }
    fn title(&self) -> &str {
        "Create resized qcow2 disk + UEFI variable store"
    }

    fn check(&self, ctx: &mut StepContext) -> Result<CheckResult> {
        let cfg = ctx.cfg.clone();
        if !cfg.instance_meta_path().exists() {
            return Ok(CheckResult::pending("instance.json is missing"));
        }
        let instance_meta = read_json(&cfg.instance_meta_path()).unwrap_or(Value::Null);
        let fingerprint = instance_meta
            .get("fingerprints")
            .and_then(|value| value.get("disk"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if fingerprint != cfg.disk_fingerprint() {
            return Ok(CheckResult::pending(
                "disk configuration changed (disk fingerprint differs)",
            ));
        }
        if !cfg.disk_path().exists() {
            return Ok(CheckResult::pending(format!(
                "missing {}",
                cfg.disk_path().display()
            )));
        }
        if !cfg.efi_vars_path().exists() {
            return Ok(CheckResult::pending(format!(
                "missing {}",
                cfg.efi_vars_path().display()
            )));
        }
        let wanted = parse_size_bytes(&cfg.disk_size)?;
        let Some(size) = disk_virtual_size(&cfg, &cfg.disk_path()) else {
            return Ok(CheckResult::pending("cannot read qcow2 metadata"));
        };
        if size != wanted {
            return Ok(CheckResult::pending(format!(
                "disk virtual size is {}, want {}",
                format_bytes(Some(size)),
                format_bytes(Some(wanted))
            )));
        }
        let efi_size = std::fs::metadata(cfg.efi_vars_path())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if efi_size < 1024 {
            return Ok(CheckResult::pending("UEFI variable store looks empty"));
        }
        Ok(CheckResult::ready(format!(
            "disk {}, UEFI vars {}",
            format_bytes(Some(size)),
            format_bytes(Some(efi_size))
        )))
    }

    fn apply(&self, ctx: &mut StepContext) -> Result<()> {
        let cfg = ctx.cfg.clone();
        if !std::fs::symlink_metadata(cfg.disk_path())
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            return message(format!(
                "The existing disk at {} needs recovery; it was kept. Use an explicit reset to create a new disk.",
                cfg.disk_path().display()
            ));
        }
        ensure_dir(&cfg.instance_dir())?;
        let base = cfg.image_path();
        let base_size = std::fs::metadata(&base)
            .map_err(|error| {
                crate::Error::Message(format!(
                    "could not read base image {}: {error}",
                    base.display()
                ))
            })?
            .len();
        let wanted = parse_size_bytes(&cfg.disk_size)?;
        check_free_space(&cfg.instance_dir(), wanted.max(base_size))?;
        cfg.save_instance_meta()?;
        if !cfg.image_checksum().matches(&base)? {
            return message(format!("base image checksum mismatch: {}", base.display()));
        }
        let engine = crate::qemu_engine::require(&cfg.home)?;
        let tmp_disk = cfg.instance_file(".disk.qcow2.tmp");
        let _ = std::fs::remove_file(&tmp_disk);
        let result = (|| -> Result<()> {
            ctx.log("copying base image into an independent qcow2 (no backing file)");
            create_disk(
                &cfg.home,
                &engine.qemu_img,
                &base,
                &tmp_disk,
                wanted,
                |argv, timeout, cwd| {
                    ctx.log(&format!("running qemu-img {}", argv[1]));
                    capture_env(argv, timeout, None, Some(cwd), None)
                },
            )?;
            let info = disk_info(&cfg, &tmp_disk).ok_or_else(|| {
                crate::Error::Message("cannot read the new qcow2 metadata".to_string())
            })?;
            if info.get("virtual-size").and_then(Value::as_u64) != Some(wanted) {
                return message("resized qcow2 has the wrong virtual size");
            }
            if !cfg.efi_vars_path().exists() {
                atomic_copy(&engine.firmware_vars, &cfg.efi_vars_path(), 0o600)?;
            }
            std::fs::rename(&tmp_disk, cfg.disk_path())?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&tmp_disk);
        result?;
        ctx.track(&cfg.disk_path(), "volume", false)?;
        ctx.track(&cfg.efi_vars_path(), "volume", false)?;
        ctx.set_meta("disk_fingerprint", json!(cfg.disk_fingerprint()));
        ctx.set_meta("virtual_size", json!(wanted));
        ctx.set_meta("base_image", json!(base.display().to_string()));
        ctx.set_meta("base_checksum", json!(cfg.image_checksum().hex()));
        ctx.set_meta(
            "efi_vars_source",
            json!(cfg.efi_vars_path().display().to_string()),
        );
        ctx.log(&format!(
            "disk ready: {} virtual",
            format_bytes(Some(wanted))
        ));
        Ok(())
    }

    fn rollback(&self, ctx: &mut StepContext, state: &Value, force: bool) -> Result<()> {
        ensure_stopped(&ctx.cfg, ctx)?;
        for leftover in [
            ctx.cfg.instance_file(".disk.qcow2.tmp"),
            ctx.cfg.instance_file(".efi-vars.fd.tmp"),
        ] {
            let _ = std::fs::remove_file(leftover);
        }
        delete_artifacts(ctx, state, force)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_a_disk_larger_than_the_target_volumes_free_space() {
        let home = std::env::temp_dir().join(format!("cabin-disk-free-{}", std::process::id()));
        let mut cfg = crate::config::Config {
            home,
            name: "test".into(),
            image_url: "https://example.test/base.img".into(),
            ..Default::default()
        };
        cfg.disk_size = (u64::MAX / 2).to_string();
        std::fs::create_dir_all(cfg.images_dir()).unwrap();
        std::fs::write(cfg.image_path(), b"base").unwrap();
        let mut ctx = StepContext::new(&cfg, "disk", "test").unwrap();
        let error = DiskStep.apply(&mut ctx).unwrap_err();
        assert!(!cfg.instance_meta_path().exists());
        assert!(!cfg.disk_path().exists());
        drop(ctx);
        std::fs::remove_dir_all(cfg.home).unwrap();
        assert!(
            error.to_string().contains("not enough free disk space"),
            "{error}"
        );
    }

    #[test]
    fn qemu_img_uses_relative_argv_from_the_unicode_data_directory() {
        let root = Path::new("Cabin 数据");
        let tool = root.join("app files").join("qemu-img");
        let image = root.join("image cache").join("base.img");
        let disk = root.join("guest files").join(".disk.qcow2.tmp");
        let mut commands = Vec::new();
        create_disk(root, &tool, &image, &disk, 6 * 1024, |argv, _, cwd| {
            assert_eq!(cwd, root);
            commands.push(argv.to_vec());
            CaptureResult {
                code: 0,
                output: String::new(),
            }
        })
        .unwrap();
        let absolute_tool = std::path::absolute(tool).unwrap();
        let tool = absolute_tool.to_str().unwrap();
        let image = image.strip_prefix(root).unwrap().to_str().unwrap();
        let disk = disk.strip_prefix(root).unwrap().to_str().unwrap();
        assert_eq!(commands[0], [tool, "convert", "-O", "qcow2", image, disk]);
        assert_eq!(commands[1], [tool, "resize", disk, "6144"]);
    }

    #[test]
    fn qemu_img_failures_keep_the_cause_and_skip_resize() {
        for (detail, expected) in [
            ("No space left on device", "not enough free disk space"),
            (
                "There is not enough space on the disk.",
                "not enough free disk space",
            ),
            (
                "Permission denied",
                "qemu-img convert failed (exit 1): Permission denied",
            ),
            (
                "The process cannot access the file because it is being used by another process",
                "being used by another process",
            ),
        ] {
            let disk = Path::new("disk.qcow2");
            let error = create_disk(
                Path::new(""),
                Path::new("qemu-img"),
                Path::new("base.img"),
                disk,
                1024,
                |argv, _, _| {
                    assert_eq!(argv[1], "convert");
                    CaptureResult {
                        code: 1,
                        output: detail.to_owned(),
                    }
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }
}
