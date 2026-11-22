//! Instance disk step: full copy of the base image, resize,
//! UEFI variable store.

use std::path::Path;

use serde_json::{Value, json};

use crate::Result;
use crate::steps::base::{
    CheckResult, Step, StepContext, delete_artifacts, disk_info, disk_virtual_size, ensure_stopped,
};
use crate::util::{chmod, ensure_dir, format_bytes, message, parse_size_bytes, read_json};
use crate::vm::{resolve_qemu, resolve_qemu_img};

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
        cfg.save_instance_meta()?;
        let base = cfg.image_path();
        if !base.exists() {
            return message(format!(
                "base image missing: {} (run the image step first)",
                base.display()
            ));
        }
        if !cfg.image_checksum().matches(&base)? {
            return message(format!("base image checksum mismatch: {}", base.display()));
        }
        let qemu_img = resolve_qemu_img(&cfg)?.display().to_string();
        let tmp_disk = cfg.instance_file(".disk.qcow2.tmp");
        let _ = std::fs::remove_file(&tmp_disk);
        let result = (|| -> Result<()> {
            ctx.log("copying base image into an independent qcow2 (no backing file)");
            ctx.run(
                &[
                    qemu_img.clone(),
                    "convert".to_string(),
                    "-O".to_string(),
                    "qcow2".to_string(),
                    "-p".to_string(),
                    base.display().to_string(),
                    tmp_disk.display().to_string(),
                ],
                std::time::Duration::from_secs(900),
                true,
            )?;
            ctx.run(
                &[
                    qemu_img.clone(),
                    "resize".to_string(),
                    tmp_disk.display().to_string(),
                    cfg.disk_size.clone(),
                ],
                std::time::Duration::from_secs(300),
                true,
            )?;
            let info = disk_info(&cfg, &tmp_disk).ok_or_else(|| {
                crate::Error::Message("cannot read the new qcow2 metadata".to_string())
            })?;
            let wanted = parse_size_bytes(&cfg.disk_size)?;
            if info.get("virtual-size").and_then(Value::as_u64) != Some(wanted) {
                return message("resized qcow2 has the wrong virtual size");
            }
            std::fs::rename(&tmp_disk, cfg.disk_path())?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&tmp_disk);
        result?;
        ctx.track(&cfg.disk_path(), "volume", false)?;
        if !cfg.efi_vars_path().exists() {
            let resolved = resolve_qemu(&cfg)?;
            atomic_copy(&resolved.firmware_vars, &cfg.efi_vars_path(), 0o600)?;
        }
        ctx.track(&cfg.efi_vars_path(), "volume", false)?;
        ctx.set_meta("disk_fingerprint", json!(cfg.disk_fingerprint()));
        ctx.set_meta("virtual_size", json!(parse_size_bytes(&cfg.disk_size)?));
        ctx.set_meta("base_image", json!(base.display().to_string()));
        ctx.set_meta("base_checksum", json!(cfg.image_checksum().hex()));
        ctx.set_meta(
            "efi_vars_source",
            json!(cfg.efi_vars_path().display().to_string()),
        );
        ctx.log(&format!(
            "disk ready: {} virtual",
            format_bytes(Some(parse_size_bytes(&cfg.disk_size)?))
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
