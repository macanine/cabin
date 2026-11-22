//! Post-provision desktop installation step (runs in place, idempotent).

use serde_json::{Value, json};

use crate::Result;
use crate::desktop;
use crate::journal::load_state;
use crate::steps::base::{CheckResult, Step, StepContext, delete_artifacts, ensure_stopped};
use crate::util::{atomic_write_json, ensure_dir, message, now_millis, read_text};
use crate::vm::{Boot, Runtime};

pub struct DesktopStep;

impl DesktopStep {
    fn wait_stopped(&self, ctx: &StepContext, runtime: &Runtime, seconds: u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while std::time::Instant::now() < deadline && runtime.alive() {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        if runtime.alive() {
            ctx.log("guest did not power off in time; forcing QEMU stop");
            runtime.stop(&ctx.log_file, std::time::Duration::from_secs(30));
        }
        runtime.cleanup_stale();
    }
}

impl Step for DesktopStep {
    fn id(&self) -> &str {
        "desktop"
    }
    fn title(&self) -> &str {
        "Install the desktop stack (Wayland/Sway/Chrome) after minimal provisioning"
    }

    fn check(&self, ctx: &mut StepContext) -> Result<CheckResult> {
        let cfg = ctx.cfg.clone();
        if !desktop::enabled(&cfg) {
            return Ok(CheckResult::ready("desktop disabled"));
        }
        let provision = load_state(&cfg.home, &cfg.name, "provision").unwrap_or(Value::Null);
        if provision.get("status").and_then(Value::as_str) != Some("applied") {
            return Ok(CheckResult::pending("provision is not applied yet"));
        }
        let state = self.state(&cfg).unwrap_or(Value::Null);
        let meta = state.get("meta").cloned().unwrap_or(Value::Null);
        if state.get("status").and_then(Value::as_str) != Some("applied") {
            return Ok(CheckResult::pending("desktop has not been installed"));
        }
        if meta.get("desktop_fingerprint").and_then(Value::as_str)
            != Some(desktop::fingerprint(&cfg).as_str())
        {
            return Ok(CheckResult::pending("desktop configuration changed"));
        }
        if !cfg.desktop_marker_path().exists() {
            return Ok(CheckResult::pending("desktop marker is missing"));
        }
        Ok(CheckResult::ready(format!(
            "desktop installed ({})",
            meta.get("desktop_env")
                .and_then(Value::as_str)
                .unwrap_or("?")
        )))
    }

    fn apply(&self, ctx: &mut StepContext) -> Result<()> {
        let cfg = ctx.cfg.clone();
        if !desktop::enabled(&cfg) {
            return Ok(());
        }
        let provision = load_state(&cfg.home, &cfg.name, "provision").unwrap_or(Value::Null);
        if provision.get("status").and_then(Value::as_str) != Some("applied") {
            return message("provision must be applied before the desktop step");
        }
        ensure_dir(&cfg.instance_dir())?;
        // Installs in place: the install script is idempotent, so a retry
        // after a failure simply runs it again on the same disk.
        ensure_stopped(&cfg, ctx)?;
        let runtime = Runtime::new(&cfg);
        let started = std::time::Instant::now();
        let result = (|| -> Result<()> {
            ctx.log(&format!(
                "booting minimal VM to install desktop: {:?}",
                cfg.desktop_env
            ));
            runtime.start(
                &ctx.log_file,
                Boot {
                    serial_log: Some(&cfg.instance_file("serial-desktop-install.log")),
                    phase: Some("install"),
                    display_mode: Some("none"),
                    ..Boot::new(std::time::Duration::from_secs(300))
                },
            )?;
            ctx.log("installing desktop packages (this can take several minutes)");
            let (rc, out) = runtime.ssh_script(
                &desktop::install_script(&cfg)?,
                std::time::Duration::from_secs(1800),
            );
            if !out.is_empty() {
                let tail: String = out
                    .chars()
                    .rev()
                    .take(16000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                ctx.log(&tail);
            }
            if rc != 0 {
                return message(format!("desktop install failed (rc={rc})"));
            }
            ctx.log("running desktop acceptance checks");
            let (rc, out) = runtime.ssh_script(
                &desktop::verify_script(&cfg),
                std::time::Duration::from_secs(120),
            );
            if !out.is_empty() {
                let tail: String = out
                    .chars()
                    .rev()
                    .take(8000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                ctx.log(&tail);
            }
            if rc != 0 {
                return message(format!("desktop verification failed (rc={rc})"));
            }
            ctx.log("shutting down");
            runtime.ssh_capture(
                "sudo -n systemctl poweroff --no-block",
                std::time::Duration::from_secs(20),
            );
            self.wait_stopped(ctx, &runtime, 180);
            let marker = json!({
                "installed": true,
                "name": cfg.name,
                "desktop": desktop::summary(&cfg),
                "duration_s": ((started.elapsed().as_secs_f64() * 10.0).round() / 10.0),
                "installed_at": now_millis(),
            });
            atomic_write_json(&cfg.desktop_marker_path(), &marker, 0o600)?;
            ctx.track_file(&cfg.desktop_marker_path())?;
            ctx.set_meta("desktop_fingerprint", json!(desktop::fingerprint(&cfg)));
            ctx.set_meta("desktop_env", json!(cfg.desktop_env));
            ctx.set_meta(
                "duration_s",
                marker.get("duration_s").cloned().unwrap_or(json!(0)),
            );
            cfg.save_instance_meta()?;
            ctx.log(&format!(
                "desktop installed in {}s",
                marker
                    .get("duration_s")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
            ));
            Ok(())
        })();
        if result.is_err() {
            let path = cfg.instance_file("serial-desktop-install.log");
            if path.exists() {
                let text = read_text(&path);
                let tail: String = text
                    .lines()
                    .rev()
                    .take(80)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                if !tail.is_empty() {
                    ctx.log(&format!("desktop serial log tail:\n{tail}"));
                }
            }
        }
        if runtime.alive() {
            runtime.stop(&ctx.log_file, std::time::Duration::from_secs(60));
        }
        result
    }

    fn rollback(&self, ctx: &mut StepContext, state: &Value, force: bool) -> Result<()> {
        let cfg = ctx.cfg.clone();
        // There is no disk baseline to return to: packages stay installed and
        // only the record is dropped, so the next apply runs the install again.
        ensure_stopped(&cfg, ctx)?;
        if cfg.desktop_marker_path().exists() {
            ctx.log(&format!("remove {}", cfg.desktop_marker_path().display()));
            let _ = std::fs::remove_file(cfg.desktop_marker_path());
        }
        delete_artifacts(ctx, state, force)?;
        ctx.log("desktop rollback complete; the next apply installs the desktop again");
        Ok(())
    }
}
