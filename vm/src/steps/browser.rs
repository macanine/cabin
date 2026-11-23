//! Browser-fingerprint step (CDP profile on top of the desktop install), in place.

use serde_json::{Value, json};

use crate::Result;
use crate::browser;
use crate::config::Config;
use crate::journal::{load_state, save_state};
use crate::steps::base::{CheckResult, Step, StepContext, delete_artifacts, ensure_stopped};
use crate::util::{atomic_write_json, ensure_dir, message, now_millis, read_text};
use crate::vm::{Boot, Runtime};

pub struct BrowserStep;

/// The guest profile's region was changed in place (`sync_environment`), so
/// the step's recorded fingerprint moves to the new configuration. Only a
/// record that was current before the change (`previous`) is moved: one that
/// was already out of date still makes the next start reinstall.
pub fn record_live_change(cfg: &Config, previous: &str) -> Result<()> {
    let Some(mut state) = load_state(&cfg.home, &cfg.name, "browser") else {
        return Ok(());
    };
    if state.get("status").and_then(Value::as_str) != Some("applied")
        || state.pointer("/meta/browser_fingerprint").and_then(Value::as_str) != Some(previous)
    {
        return Ok(());
    }
    state["meta"]["browser_fingerprint"] = json!(browser::fingerprint(cfg)?);
    save_state(&cfg.home, &cfg.name, "browser", &state)?;
    Ok(())
}

impl BrowserStep {
    fn wait_stopped(&self, ctx: &StepContext, runtime: &Runtime) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
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

impl Step for BrowserStep {
    fn id(&self) -> &str {
        "browser"
    }
    fn title(&self) -> &str {
        "Install the CDP browser fingerprint profile (Canvas/WebGL/Audio/UA/timezone)"
    }

    fn check(&self, ctx: &mut StepContext) -> Result<CheckResult> {
        let cfg = ctx.cfg.clone();
        if !browser::enabled(&cfg) {
            return Ok(CheckResult::ready("browser fingerprint disabled"));
        }
        let desktop = load_state(&cfg.home, &cfg.name, "desktop").unwrap_or(Value::Null);
        if desktop.get("status").and_then(Value::as_str) != Some("applied") {
            return Ok(CheckResult::pending("desktop is not applied yet"));
        }
        let state = self.state(&cfg).unwrap_or(Value::Null);
        let meta = state.get("meta").cloned().unwrap_or(Value::Null);
        if state.get("status").and_then(Value::as_str) != Some("applied") {
            return Ok(CheckResult::pending(
                "browser fingerprint has not been applied",
            ));
        }
        let saved = meta.get("browser_fingerprint").and_then(Value::as_str);
        if saved != Some(browser::fingerprint(&cfg)?.as_str()) {
            let detail = if saved == Some(browser::legacy_fingerprint(&cfg)?.as_str()) {
                "browser controller requires native migration"
            } else {
                "browser fingerprint configuration changed"
            };
            return Ok(CheckResult::pending(detail));
        }
        if !cfg.browser_marker_path().exists() {
            return Ok(CheckResult::pending("browser marker is missing"));
        }
        let fingerprint = meta
            .get("browser_fingerprint")
            .and_then(Value::as_str)
            .unwrap_or("");
        Ok(CheckResult::ready(format!(
            "browser profile applied ({})",
            &fingerprint[..fingerprint.len().min(12)]
        )))
    }

    fn apply(&self, ctx: &mut StepContext) -> Result<()> {
        let cfg = ctx.cfg.clone();
        if !browser::enabled(&cfg) {
            return Ok(());
        }
        let desktop = load_state(&cfg.home, &cfg.name, "desktop").unwrap_or(Value::Null);
        if desktop.get("status").and_then(Value::as_str) != Some("applied") {
            return message("desktop must be applied before the browser fingerprint step");
        }
        // Installs in place; the profile script overwrites its own files.
        ensure_dir(&cfg.instance_dir())?;
        ensure_stopped(&cfg, ctx)?;
        let data = browser::resolve(&cfg)?;
        let install_data = browser::install_profile(&data);
        let runtime = Runtime::new(&cfg);
        let started = std::time::Instant::now();
        let result = (|| -> Result<()> {
            ctx.log("booting desktop VM to install the browser fingerprint profile");
            runtime.start(
                &ctx.log_file,
                Boot {
                    serial_log: Some(&cfg.instance_logs_dir().join("serial-browser-install.log")),
                    phase: Some("install"),
                    display_mode: Some("none"),
                    ..Boot::new(std::time::Duration::from_secs(300))
                },
            )?;
            crate::agentd::ensure_helper(&runtime, ctx.helper_binary()?)?;
            ctx.log("writing browser profile, JS overrides and Chrome policy");
            let (rc, out) = runtime.ssh_script(
                &browser::install_script(&cfg, &install_data)?,
                std::time::Duration::from_secs(600),
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
                return message(format!("browser fingerprint installation failed (rc={rc})"));
            }
            let (rc, out) = runtime.ssh_script(
                &browser::verify_script(&cfg, Some(&install_data))?,
                std::time::Duration::from_secs(120),
            );
            if !out.is_empty() {
                let tail: String = out
                    .chars()
                    .rev()
                    .take(4000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                ctx.log(&tail);
            }
            if rc != 0 {
                return message(format!(
                    "browser fingerprint file verification failed (rc={rc})"
                ));
            }
            ctx.log("shutting down");
            runtime.ssh_capture(
                "sudo -n systemctl poweroff --no-block",
                std::time::Duration::from_secs(20),
            );
            self.wait_stopped(ctx, &runtime);
            let marker = json!({
                "installed": true,
                "name": cfg.name,
                "browser": browser::summary(&cfg, Some(&data))?,
                "duration_s": ((started.elapsed().as_secs_f64() * 10.0).round() / 10.0),
                "installed_at": now_millis(),
            });
            atomic_write_json(&cfg.browser_marker_path(), &marker, 0o600)?;
            ctx.track_file(&cfg.browser_marker_path())?;
            ctx.set_meta("browser_fingerprint", json!(browser::fingerprint(&cfg)?));
            ctx.set_meta(
                "duration_s",
                marker.get("duration_s").cloned().unwrap_or(json!(0)),
            );
            cfg.save_instance_meta()?;
            ctx.log(&format!(
                "browser fingerprint installed in {}s",
                marker
                    .get("duration_s")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
            ));
            Ok(())
        })();
        if result.is_err() {
            let path = cfg.instance_logs_dir().join("serial-browser-install.log");
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
                    ctx.log(&format!("browser serial log tail:\n{tail}"));
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
        // No disk baseline: the profile stays on disk; dropping the record
        // makes the next apply write it again.
        ensure_stopped(&cfg, ctx)?;
        if cfg.browser_marker_path().exists() {
            ctx.log(&format!("remove {}", cfg.browser_marker_path().display()));
            let _ = std::fs::remove_file(cfg.browser_marker_path());
        }
        delete_artifacts(ctx, state, force)?;
        ctx.log("browser rollback complete; the next apply writes the profile again");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::record_live_change;
    use crate::browser;
    use crate::journal::{load_state, save_state};
    use serde_json::json;

    struct Home(std::path::PathBuf);
    impl Home {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("browser-step-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn config(home: &std::path::Path, timezone: &str) -> crate::config::Config {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        crate::config::build_config(
            &json!({
                "home": home,
                "desktop_env": "sway",
                "desktop_chrome": true,
                "desktop_resolution": "1600x900",
                "browser": {"enabled": true, "sync_with_system": true, "timezone": timezone, "locale": "en_US.UTF-8"}
            }),
            root,
        )
        .unwrap()
    }

    #[test]
    fn a_live_region_change_keeps_the_step_current() {
        let home = Home::new("current");
        let before = config(home.path(), "Asia/Tokyo");
        let after = config(home.path(), "Europe/Berlin");
        let old = browser::fingerprint(&before).unwrap();
        assert_ne!(old, browser::fingerprint(&after).unwrap(), "the region is part of the fingerprint");
        save_state(&after.home, &after.name, "browser", &json!({"status": "applied", "meta": {"browser_fingerprint": old}})).unwrap();

        record_live_change(&after, &old).unwrap();
        let state = load_state(&after.home, &after.name, "browser").unwrap();
        assert_eq!(state["meta"]["browser_fingerprint"], json!(browser::fingerprint(&after).unwrap()));
    }

    #[test]
    fn an_already_stale_record_is_left_for_the_next_start() {
        let home = Home::new("stale");
        let before = config(home.path(), "Asia/Tokyo");
        let after = config(home.path(), "Europe/Berlin");
        let old = browser::fingerprint(&before).unwrap();
        save_state(&after.home, &after.name, "browser", &json!({"status": "applied", "meta": {"browser_fingerprint": "stale"}})).unwrap();

        record_live_change(&after, &old).unwrap();
        let state = load_state(&after.home, &after.name, "browser").unwrap();
        assert_eq!(state["meta"]["browser_fingerprint"], "stale");
    }
}
