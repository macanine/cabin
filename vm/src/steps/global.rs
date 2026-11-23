//! Global steps: the built-in QEMU and the OS image download.

use std::fs::{self, Metadata};

use serde_json::{Value, json};

use crate::Result;
use crate::steps::base::{CheckResult, Step, StepContext, delete_artifacts};
use crate::util::{capture, ensure_dir, format_bytes, message};

/// Verifies the QEMU bundled with the app (`qemu_engine`, `vendor`): it runs
/// `qemu-img` and the system binary. Only a macOS development build without a
/// vendor directory downloads the pinned macanine/qemu build instead.
pub struct QemuStep;

/// A bundled engine that is present but does not run is a damaged install.
fn damaged(detail: &str) -> String {
    if crate::vendor::vendor().is_some() {
        format!("{} ({detail})", crate::qemu_engine::BUNDLED_MISSING)
    } else {
        detail.to_string()
    }
}

impl Step for QemuStep {
    fn id(&self) -> &str {
        "qemu"
    }
    fn title(&self) -> &str {
        "Verify the bundled QEMU and UEFI firmware"
    }
    fn scope(&self) -> &str {
        "global"
    }

    fn check(&self, ctx: &mut StepContext) -> Result<CheckResult> {
        let Some(engine) = crate::qemu_engine::installed(&ctx.cfg.home) else {
            return Ok(CheckResult::pending(
                crate::vendor::vendor_problem().unwrap_or_else(|| {
                    if cfg!(target_os = "macos") && !cfg!(debug_assertions) {
                        crate::qemu_engine::BUNDLED_MISSING.to_string()
                    } else {
                        format!(
                            "QEMU is not available ({})",
                            crate::qemu_engine::RELEASE
                        )
                    }
                }),
            ));
        };
        let (version, image) = std::thread::scope(|scope| {
            let version = std::thread::Builder::new()
                .name("qemu-version".into())
                .spawn_scoped(scope, || crate::qemu_engine::version(&engine));
            let image = capture(
                &[
                    engine.qemu_img.display().to_string(),
                    "--version".to_string(),
                ],
                std::time::Duration::from_secs(10),
                None,
            );
            let version = (|| -> Result<String> {
                version?.join().map_err(|_| {
                    crate::Error::Message("QEMU version worker stopped unexpectedly".into())
                })?
            })();
            (version, image)
        });
        let version = match version {
            Ok(version) => version,
            Err(error) => return Ok(CheckResult::pending(damaged(&error.to_string()))),
        };
        if image.code != 0 {
            return Ok(CheckResult::pending(damaged("qemu-img does not run")));
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
        let version = crate::qemu_engine::version(&engine)
            .map_err(|error| crate::Error::Message(damaged(&error.to_string())))?;
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
    let modified = metadata
        .modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|at| (at.as_secs(), at.subsec_nanos()));
    json!({
        "algorithm": checksum.name(),
        "checksum": checksum.hex(),
        "id": crate::storage::file_id(metadata),
        "size": metadata.len(),
        "modified": modified,
        "changed": crate::storage::changed_time(metadata),
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
            let image = cfg.image_path();
            let stage = image.with_extension("embedded.tmp");
            let result = (|| -> Result<_> {
                fs::copy(&bundled, &stage).map_err(|error| {
                    let detail = if error.kind() == std::io::ErrorKind::StorageFull {
                        "not enough free space to copy the embedded image".to_owned()
                    } else {
                        format!("copy embedded image failed: {error}")
                    };
                    crate::Error::Message(detail)
                })?;
                let actual = expected.of_file(&stage)?;
                if actual != expected.hex() {
                    return message(format!("embedded image checksum mismatch: {actual}"));
                }
                fs::rename(&stage, &image)?;
                Ok(verified_image_identity(&fs::metadata(&image)?, &expected))
            })();
            if result.is_err() {
                let _ = fs::remove_file(&stage);
            }
            let identity = result?;
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
        let part = crate::download::partial_path(&cfg.image_path());
        let candidates = crate::os::order_by_latency(
            crate::os::image_candidates(&cfg.image_url),
            crate::os::probe_latency,
        );
        ctx.log(&format!("download sources: {}", candidates.join(", ")));
        let result = crate::download::download_first(
            &candidates,
            &cfg.image_path(),
            &expected,
            &ctx.log_file,
        );
        if result.is_err() && part.exists() {
            let _ = ctx.track(&part, "file", false);
        }
        let used = result?;
        ctx.track_file(&cfg.image_path())?;
        ctx.set_meta("source", json!("download"));
        ctx.set_meta("url", json!(used));
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
