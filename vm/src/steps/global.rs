//! Global steps: the built-in QEMU and the OS image download.

use std::fs::{self, Metadata};
use std::os::unix::fs::MetadataExt;

use serde_json::{Value, json};

use crate::Result;
use crate::steps::base::{CheckResult, Step, StepContext, delete_artifacts};
use crate::util::{capture, ensure_dir, format_bytes, message};

/// The built-in QEMU (`qemu_engine`): the pinned macanine/qemu build with
/// its firmware. Never PATH or Homebrew.
pub struct QemuStep;

impl Step for QemuStep {
    fn id(&self) -> &str {
        "qemu"
    }
    fn title(&self) -> &str {
        "Install the built-in QEMU and UEFI firmware"
    }
    fn scope(&self) -> &str {
        "global"
    }

    fn check(&self, ctx: &mut StepContext) -> Result<CheckResult> {
        let Some(engine) = crate::qemu_engine::installed(&ctx.cfg.home) else {
            return Ok(CheckResult::pending(format!(
                "built-in QEMU {} is not installed",
                crate::qemu_engine::RELEASE
            )));
        };
        let (version, image) = std::thread::scope(|scope| {
            let version = scope.spawn(|| crate::qemu_engine::version(&engine));
            let image = capture(
                &[
                    engine.qemu_img.display().to_string(),
                    "--version".to_string(),
                ],
                std::time::Duration::from_secs(10),
                None,
            );
            (version.join().expect("QEMU version worker panicked"), image)
        });
        let version = match version {
            Ok(version) => version,
            Err(error) => return Ok(CheckResult::pending(error.to_string())),
        };
        if image.code != 0 {
            return Ok(CheckResult::pending("qemu-img does not run"));
        }
        Ok(CheckResult::ready(format!(
            "{version}; firmware + qemu-img present"
        )))
    }

    fn apply(&self, ctx: &mut StepContext) -> Result<()> {
        let home = ctx.cfg.home.clone();
        let (engine, created) = crate::qemu_engine::install(&home, &ctx.log_file)?;
        for path in &created {
            ctx.track_file(path)?;
        }
        let version = crate::qemu_engine::version(&engine)?;
        ctx.log(&version);
        ctx.log(&format!(
            "UEFI firmware: {}",
            engine.firmware_code.display()
        ));
        ctx.set_meta("release", json!(crate::qemu_engine::RELEASE));
        ctx.set_meta("sha256", json!(crate::qemu_engine::SHA256));
        ctx.set_meta("qemu_bin", json!(engine.qemu.display().to_string()));
        ctx.set_meta("qemu_version", json!(version));
        Ok(())
    }

    fn rollback(&self, ctx: &mut StepContext, state: &Value, force: bool) -> Result<()> {
        delete_artifacts(ctx, state, force)
    }
}

pub struct ImageStep;

fn image_verification_path(image: &std::path::Path) -> std::path::PathBuf {
    let name = image
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("image");
    image.with_file_name(format!(".{name}.verified.json"))
}

fn remember_verified_image(image: &std::path::Path, identity: &Value) {
    let _ = crate::util::atomic_write_json(&image_verification_path(image), identity, 0o600);
}

fn verified_image_identity(metadata: &Metadata, checksum: &crate::download::Checksum) -> Value {
    json!({
        "algorithm": checksum.name(),
        "checksum": checksum.hex(),
        "device": metadata.dev(),
        "inode": metadata.ino(),
        "size": metadata.len(),
        "modified": [metadata.mtime(), metadata.mtime_nsec()],
        "changed": [metadata.ctime(), metadata.ctime_nsec()],
    })
}

impl Step for ImageStep {
    fn id(&self) -> &str {
        "image"
    }
    fn title(&self) -> &str {
        "Download and verify the OS cloud image"
    }
    fn scope(&self) -> &str {
        "global"
    }

    fn check(&self, ctx: &mut StepContext) -> Result<CheckResult> {
        if !ctx.cfg.image_pinned() {
            return Ok(CheckResult::pending("no image build is pinned yet"));
        }
        let image = ctx.cfg.image_path();
        let metadata = match fs::metadata(&image) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                return Ok(CheckResult::pending(format!(
                    "not a file: {}",
                    image.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CheckResult::pending(format!("missing {}", image.display())));
            }
            Err(error) => return Err(error.into()),
        };
        let expected = ctx.cfg.image_checksum();
        let name = image
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("image");
        let cache = image_verification_path(&image);
        let identity = verified_image_identity(&metadata, &expected);
        let reused =
            ctx.action == "start" && crate::util::read_json(&cache).as_ref() == Some(&identity);
        if !reused {
            let actual = expected.of_file(&image)?;
            if actual != expected.hex() {
                return Ok(CheckResult::pending(format!("checksum mismatch: {actual}")));
            }
            let after = fs::metadata(&image)?;
            if verified_image_identity(&after, &expected) != identity {
                return crate::util::message("The OS image changed during checksum verification");
            }
            if matches!(ctx.action.as_str(), "start" | "apply") {
                remember_verified_image(&image, &identity);
            }
        }
        Ok(CheckResult::ready(format!(
            "{name} ({}) {} {}",
            format_bytes(Some(metadata.len())),
            expected.name(),
            if reused {
                "previously verified"
            } else {
                "verified"
            }
        )))
    }

    fn apply(&self, ctx: &mut StepContext) -> Result<()> {
        let cfg = ctx.cfg.clone();
        if !cfg.image_pinned() {
            return message("no image build is pinned; reinstall to choose the operating system");
        }
        let expected = cfg.image_checksum();
        ctx.log(&format!("{}: {}", cfg.guest_os().name, cfg.image_url));
        ensure_dir(&cfg.images_dir())?;
        let bundled = cfg.bundled_image_path();
        if bundled.is_file() {
            ctx.log(&format!("using embedded image: {}", bundled.display()));
            std::fs::copy(&bundled, cfg.image_path()).map_err(|error| {
                crate::Error::Message(format!("copy embedded image failed: {error}"))
            })?;
            let identity = verified_image_identity(&fs::metadata(cfg.image_path())?, &expected);
            let actual = expected.of_file(&cfg.image_path())?;
            if actual != expected.hex() {
                let _ = std::fs::remove_file(cfg.image_path());
                return message(format!("embedded image checksum mismatch: {actual}"));
            }
            ctx.track_file(&cfg.image_path())?;
            if verified_image_identity(&fs::metadata(cfg.image_path())?, &expected) == identity {
                remember_verified_image(&cfg.image_path(), &identity);
            }
            ctx.set_meta("source", json!("embedded"));
            ctx.set_meta("url", json!(cfg.image_url));
            ctx.set_meta("checksum", json!(expected.hex()));
            ctx.set_meta(
                "size",
                json!(
                    std::fs::metadata(cfg.image_path())
                        .map(|metadata| metadata.len())
                        .unwrap_or(0)
                ),
            );
            return Ok(());
        }
        let part = cfg.image_path().with_file_name(format!(
            "{}.part",
            cfg.image_path()
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("image")
        ));
        let result = crate::download::download_verified(
            &cfg.image_url,
            &cfg.image_path(),
            &expected,
            &ctx.log_file,
        );
        if result.is_err() && part.exists() {
            let _ = ctx.track(&part, "file", false);
        }
        result?;
        ctx.track_file(&cfg.image_path())?;
        ctx.set_meta("source", json!("download"));
        ctx.set_meta("url", json!(cfg.image_url));
        ctx.set_meta("checksum", json!(expected.hex()));
        ctx.set_meta(
            "size",
            json!(
                std::fs::metadata(cfg.image_path())
                    .map(|metadata| metadata.len())
                    .unwrap_or(0)
            ),
        );
        Ok(())
    }

    fn rollback(&self, ctx: &mut StepContext, state: &Value, force: bool) -> Result<()> {
        delete_artifacts(ctx, state, force)
    }
}
