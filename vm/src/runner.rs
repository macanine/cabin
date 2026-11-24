//! Pipeline runner: plan, apply with auto-rollback, rollback, status, lifecycle.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::Result;
use crate::config::{Config, load_instance_meta};
use crate::device::render as device_render;
use crate::device::verify as device_verify;
use crate::journal::Journal;
use crate::lock::FileLock;
use crate::paths::StatePaths;
use crate::proxy;
use crate::startup::{Attempt, Phase};
use crate::steps::base::{CheckResult, Step, StepContext, ensure_stopped};
use crate::steps::browser::BrowserStep;
use crate::steps::desktop::DesktopStep;
use crate::steps::device::{DeviceGroupStep, DeviceSealStep};
use crate::steps::disk::DiskStep;
use crate::steps::global::{ImageStep, QemuStep};
use crate::steps::provision::ProvisionStep;
use crate::steps::seed::SeedStep;
use crate::util::{
    atomic_write_bytes, atomic_write_json, ensure_dir, localstamp, message, now_millis, read_json,
};
use crate::vm::{Boot, Runtime, preflight, preflight_warnings};

pub fn all_steps() -> Vec<Box<dyn Step>> {
    vec![
        Box::new(QemuStep),
        Box::new(ImageStep),
        Box::new(DiskStep),
        Box::new(SeedStep),
        Box::new(DeviceGroupStep::new("cpu", "cpu.json")),
        Box::new(DeviceGroupStep::new("smbios", "smbios.json")),
        Box::new(DeviceGroupStep::new("devices", "devices.json")),
        Box::new(DeviceGroupStep::new("network", "network.json")),
        Box::new(DeviceGroupStep::new("guest", "guest.json")),
        Box::new(ProvisionStep),
        Box::new(DesktopStep),
        Box::new(BrowserStep),
        Box::new(DeviceSealStep),
    ]
}

struct ReconfigureChanges<'a> {
    old_user: &'a str,
    new_user: &'a str,
    old_host: &'a str,
    new_host: &'a str,
    old_timezone: &'a str,
    new_timezone: &'a str,
    old_locale: &'a str,
    new_locale: &'a str,
}

pub struct Runner {
    pub cfg: Config,
    pub journal: Journal,
    pub steps: Vec<Box<dyn Step>>,
    agentd_binary: PathBuf,
}

impl Runner {
    pub fn new(cfg: Config) -> Result<Self> {
        let journal = Journal::new(&cfg.home)?;
        Ok(Self {
            cfg,
            journal,
            steps: all_steps(),
            agentd_binary: crate::agentd::default_binary(),
        })
    }

    pub fn with_agentd(mut self, binary: impl Into<PathBuf>) -> Self {
        self.agentd_binary = binary.into();
        self
    }

    fn ensure_helper(&self, runtime: &Runtime) -> Result<()> {
        crate::agentd::ensure_helper(runtime, &self.agentd_binary)
    }

    fn require_exclusive_operation_lock(&self, lock: &FileLock, operation: &str) -> Result<()> {
        if lock.path() != StatePaths::new(&self.cfg.home).operation_lock() || !lock.is_exclusive() {
            return message(format!(
                "{operation} requires the exclusive instance home operation lock"
            ));
        }
        Ok(())
    }

    pub fn visible_step_ids(&self) -> Vec<String> {
        self.steps
            .iter()
            .filter(|step| {
                if !self.cfg.device_enabled() && step.id().starts_with("device-") {
                    return false;
                }
                if self.cfg.desktop_env.is_none() && step.id() == "desktop" {
                    return false;
                }
                if !self.cfg.browser_enabled() && step.id() == "browser" {
                    return false;
                }
                true
            })
            .map(|step| step.id().to_string())
            .collect()
    }

    fn check(&self, step: &dyn Step, action: &str) -> Result<(CheckResult, StepContext)> {
        let mut ctx = StepContext::new(&self.cfg, step.id(), action)?;
        let result = step.check(&mut ctx)?;
        Ok((result, ctx))
    }

    pub fn preflight(&self) -> Result<()> {
        let cfg = &self.cfg;
        ensure_dir(&cfg.instance_logs_dir())?;
        let log_path = cfg
            .instance_logs_dir()
            .join(format!("preflight-{}.log", localstamp()));
        let issues = preflight(cfg);
        let warnings = preflight_warnings(cfg);
        let mut text = format!("# preflight instance={} at={}\n", cfg.name, now_millis());
        for warning in &warnings {
            text.push_str(&format!("WARN {warning}\n"));
        }
        if issues.is_empty() {
            text.push_str("OK\n");
        } else {
            for issue in &issues {
                text.push_str(&format!("FAIL {issue}\n"));
            }
        }
        cabin_diagnostics::open_capture(&log_path, cabin_diagnostics::Capture::Replace)?
            .write_all(text.as_bytes())?;
        let mut fields = Map::new();
        fields.insert("instance".to_string(), json!(cfg.name));
        fields.insert("ok".to_string(), json!(issues.is_empty()));
        fields.insert("issues".to_string(), json!(issues));
        fields.insert("log".to_string(), json!(log_path.display().to_string()));
        self.journal.append("preflight", fields)?;
        for warning in &warnings {
            println!("[preflight] warning: {warning}");
        }
        if issues.is_empty() {
            println!("[preflight] ok ({})", log_path.display());
            Ok(())
        } else {
            message(format!(
                "preflight failed:\n  - {}\nlog: {}",
                issues.join("\n  - "),
                log_path.display()
            ))
        }
    }

    fn config_conflicts(&self) -> Result<()> {
        let cfg = &self.cfg;
        let Some(existing) = load_instance_meta(&cfg.home, &cfg.name) else {
            return Ok(());
        };
        let fingerprints = existing.get("fingerprints").cloned().unwrap_or(Value::Null);
        if fingerprints.get("overall").and_then(Value::as_str) == Some(cfg.fingerprint().as_str()) {
            return Ok(());
        }
        let provision = self
            .steps
            .iter()
            .find(|step| step.id() == "provision")
            .and_then(|step| step.state(cfg))
            .unwrap_or(Value::Null);
        let disk = self
            .steps
            .iter()
            .find(|step| step.id() == "disk")
            .and_then(|step| step.state(cfg))
            .unwrap_or(Value::Null);
        let runtime = Runtime::new(cfg);
        if provision.get("status").and_then(Value::as_str) == Some("applied") || runtime.running() {
            return message(format!(
                "instance {:?} is already provisioned with a different configuration.\nUse a new --name, or destroy it first:\n  vmctl destroy --name {} --yes",
                cfg.name, cfg.name
            ));
        }
        if disk.get("status").and_then(Value::as_str) == Some("applied")
            && fingerprints.get("disk").and_then(Value::as_str)
                != Some(cfg.disk_fingerprint().as_str())
        {
            return message(format!(
                "disk configuration (disk size or image) changed after the disk was created.\nRoll it back first: vmctl rollback --name {} --step disk",
                cfg.name
            ));
        }
        println!("[config] pre-provision configuration changed; seed will be regenerated");
        Ok(())
    }

    fn rollback_step(&self, step: &dyn Step, state: &Value, force: bool) -> Result<()> {
        let cfg = &self.cfg;
        let mut ctx = StepContext::new(cfg, step.id(), "rollback")?;
        let mut fields = Map::new();
        fields.insert("instance".to_string(), json!(cfg.name));
        fields.insert("step".to_string(), json!(step.id()));
        self.journal.append("rollback_start", fields)?;
        ctx.log(&format!("rolling back step {}", step.id()));
        let result = step.rollback(&mut ctx, state, force);
        match result {
            Ok(()) => {
                step.mark_rolled_back(cfg, &ctx, state, true, None)?;
                let mut fields = Map::new();
                fields.insert("instance".to_string(), json!(cfg.name));
                fields.insert("step".to_string(), json!(step.id()));
                fields.insert("log".to_string(), json!(ctx.log_path.display().to_string()));
                self.journal.append("rollback_ok", fields)?;
                ctx.log(&format!("step {} rolled back", step.id()));
                ctx.close();
                Ok(())
            }
            Err(error) => {
                let text = error.to_string();
                step.mark_rolled_back(cfg, &ctx, state, false, Some(&text))?;
                let mut fields = Map::new();
                fields.insert("instance".to_string(), json!(cfg.name));
                fields.insert("step".to_string(), json!(step.id()));
                fields.insert("error".to_string(), json!(text));
                fields.insert("log".to_string(), json!(ctx.log_path.display().to_string()));
                self.journal.append("rollback_failed", fields)?;
                ctx.log(&format!("rollback failed: {text}"));
                ctx.close();
                message(format!(
                    "rollback of step {} failed: {text}\nlog: {}",
                    step.id(),
                    ctx.log_path.display()
                ))
            }
        }
    }

    // ---- public: plan / apply / rollback ------------------------------
    pub fn plan(&self) -> Result<Vec<Value>> {
        let lock = FileLock::acquire_shared(&StatePaths::new(&self.cfg.home).operation_lock())?;
        self.plan_locked(&lock)
    }

    /// Produce a plan while the caller holds this home's operation lock.
    pub fn plan_locked(&self, lock: &FileLock) -> Result<Vec<Value>> {
        if lock.path() != StatePaths::new(&self.cfg.home).operation_lock() {
            return message("plan requires the instance home operation lock");
        }
        let mut rows = Vec::new();
        for step in &self.steps {
            if !self.visible_step_ids().contains(&step.id().to_string()) {
                continue;
            }
            let (result, ctx) = self.check(step.as_ref(), "plan")?;
            let state = step.state(&self.cfg).unwrap_or(Value::Null);
            rows.push(json!({
                "id": step.id(),
                "title": step.title(),
                "scope": if step.scope() == "global" { "global" } else { "instance" },
                "ok": result.ok,
                "detail": result.detail,
                "state": state.get("status").and_then(Value::as_str).unwrap_or("new"),
                "log": state.get("log").cloned().unwrap_or(Value::Null),
            }));
            ctx.close();
        }
        Ok(rows)
    }

    pub fn apply(&self) -> Result<()> {
        let lock = FileLock::acquire(&StatePaths::new(&self.cfg.home).operation_lock())?;
        self.apply_locked(&lock)
    }

    /// Apply while the caller holds this home's exclusive operation lock.
    pub fn apply_locked(&self, lock: &FileLock) -> Result<()> {
        self.apply_steps_locked(lock, None)
    }

    pub(crate) fn apply_for_start_locked(&self, lock: &FileLock, attempt: &Attempt) -> Result<()> {
        self.apply_steps_locked(lock, Some(attempt))
    }

    fn apply_steps_locked(&self, lock: &FileLock, attempt: Option<&Attempt>) -> Result<()> {
        self.require_exclusive_operation_lock(lock, "apply")?;
        let cfg = &self.cfg;
        let lifecycle = crate::vm::Lifecycle::for_home(&cfg.home);
        lifecycle.check()?;
        self.preflight()?;
        self.config_conflicts()?;
        if let Some(attempt) = attempt {
            attempt.complete(Phase::Preflight, "Host and machine configuration are ready");
            attempt.begin(Phase::Install);
        }
        let mut fields = Map::new();
        fields.insert("instance".to_string(), json!(cfg.name));
        fields.insert("fingerprint".to_string(), json!(cfg.fingerprint()));
        self.journal.append("apply_start", fields)?;
        for step in &self.steps {
            lifecycle.check()?;
            let mut ctx = StepContext::new(
                cfg,
                step.id(),
                if attempt.is_some() { "start" } else { "apply" },
            )?;
            ctx.agentd_binary = Some(self.agentd_binary.clone());
            if let Some(attempt) = attempt {
                attempt.note(&format!(
                    "Checking {} ({})",
                    step.id(),
                    ctx.log_path.display()
                ));
            }
            let mut fields = Map::new();
            fields.insert("instance".to_string(), json!(cfg.name));
            fields.insert("step".to_string(), json!(step.id()));
            self.journal.append("step_start", fields)?;
            let outcome = (|| -> Result<()> {
                let result = step.check(&mut ctx)?;
                if let Some(attempt) = attempt {
                    attempt.note(&format!(
                        "{} checked in {:.3}s · {}",
                        step.id(),
                        ctx.elapsed(),
                        result.detail
                    ));
                }
                if result.ok {
                    ctx.log(&format!("already satisfied: {}", result.detail));
                    let mut fields = Map::new();
                    fields.insert("instance".to_string(), json!(cfg.name));
                    fields.insert("step".to_string(), json!(step.id()));
                    fields.insert("detail".to_string(), json!(result.detail));
                    self.journal.append("step_skip", fields)?;
                    return Ok(());
                }
                ctx.log(&format!("not satisfied: {}", result.detail));
                ctx.log(&format!("applying: {}", step.title()));
                lifecycle.check()?;
                step.apply(&mut ctx)?;
                step.save_applied(cfg, &ctx, "applied", None)?;
                let mut fields = Map::new();
                fields.insert("instance".to_string(), json!(cfg.name));
                fields.insert("step".to_string(), json!(step.id()));
                fields.insert(
                    "duration_s".to_string(),
                    json!((ctx.elapsed() * 100.0).round() / 100.0),
                );
                fields.insert("log".to_string(), json!(ctx.log_path.display().to_string()));
                self.journal.append("step_ok", fields)?;
                ctx.log(&format!(
                    "step {} applied in {:.1}s",
                    step.id(),
                    ctx.elapsed()
                ));
                Ok(())
            })();
            match outcome {
                Ok(()) => {
                    ctx.close();
                }
                Err(error) => {
                    let text = error.to_string();
                    step.save_applied(cfg, &ctx, "failed", Some(&text))?;
                    let mut fields = Map::new();
                    fields.insert("instance".to_string(), json!(cfg.name));
                    fields.insert("step".to_string(), json!(step.id()));
                    fields.insert("error".to_string(), json!(text));
                    fields.insert("log".to_string(), json!(ctx.log_path.display().to_string()));
                    self.journal.append("step_failed", fields)?;
                    ctx.log(&format!("step failed: {text}"));
                    ctx.close();
                    let mut rollback_ctx = StepContext::new(cfg, step.id(), "rollback")?;
                    rollback_ctx.log("automatic rollback after failure");
                    let current_state = step.state(cfg).unwrap_or(Value::Null);
                    let rollback_result = step.rollback(&mut rollback_ctx, &current_state, false);
                    match rollback_result {
                        Ok(()) => {
                            step.mark_rolled_back(cfg, &rollback_ctx, &current_state, true, None)?;
                            let mut fields = Map::new();
                            fields.insert("instance".to_string(), json!(cfg.name));
                            fields.insert("step".to_string(), json!(step.id()));
                            fields.insert(
                                "log".to_string(),
                                json!(rollback_ctx.log_path.display().to_string()),
                            );
                            self.journal.append("auto_rollback_ok", fields)?;
                            rollback_ctx
                                .log(&format!("step {} automatically rolled back", step.id()));
                        }
                        Err(rollback_error) => {
                            let rollback_text = rollback_error.to_string();
                            step.mark_rolled_back(
                                cfg,
                                &rollback_ctx,
                                &current_state,
                                false,
                                Some(&rollback_text),
                            )?;
                            let mut fields = Map::new();
                            fields.insert("instance".to_string(), json!(cfg.name));
                            fields.insert("step".to_string(), json!(step.id()));
                            fields.insert("error".to_string(), json!(rollback_text));
                            fields.insert(
                                "log".to_string(),
                                json!(rollback_ctx.log_path.display().to_string()),
                            );
                            self.journal.append("auto_rollback_failed", fields)?;
                            rollback_ctx
                                .log(&format!("automatic rollback failed: {rollback_text}"));
                        }
                    }
                    rollback_ctx.close();
                    return message(format!(
                        "step {:?} failed: {text}\nstep log: {}\nrollback log: {}",
                        step.id(),
                        ctx.log_path.display(),
                        rollback_ctx.log_path.display()
                    ));
                }
            }
        }
        cfg.save_instance_meta()?;
        let mut fields = Map::new();
        fields.insert("instance".to_string(), json!(cfg.name));
        fields.insert("failures".to_string(), json!(0));
        self.journal.append("apply_done", fields)?;
        let credentials = read_json(&cfg.credentials_path()).unwrap_or(Value::Null);
        if credentials.is_object()
            && (credentials.get("username").and_then(Value::as_str)
                != Some(cfg.current_username().as_str())
                || credentials.get("hostname").and_then(Value::as_str)
                    != Some(cfg.current_hostname().as_str()))
        {
            let mut credentials = credentials;
            if let Some(map) = credentials.as_object_mut() {
                map.insert("username".to_string(), json!(cfg.current_username()));
                map.insert("hostname".to_string(), json!(cfg.current_hostname()));
            }
            atomic_write_json(&cfg.credentials_path(), &credentials, 0o600)?;
        }
        if let Some(attempt) = attempt {
            attempt.complete(Phase::Install, "Installation validated");
            return Ok(());
        }
        let vm_running = Runtime::new(cfg).running();
        if vm_running {
            println!("\n[apply] instance {} is ready; VM is running", cfg.name);
        } else {
            println!(
                "\n[apply] instance {} is ready (VM stopped). Start it with: vmctl start --name {}",
                cfg.name, cfg.name
            );
        }
        Ok(())
    }

    pub fn rollback(&self, step_id: Option<&str>, last: bool, force: bool) -> Result<Vec<String>> {
        let lock = FileLock::acquire(&StatePaths::new(&self.cfg.home).operation_lock())?;
        self.rollback_locked(&lock, step_id, last, force)
    }

    pub fn rollback_locked(
        &self,
        lock: &FileLock,
        step_id: Option<&str>,
        last: bool,
        force: bool,
    ) -> Result<Vec<String>> {
        let cfg = &self.cfg;
        if lock.path() != StatePaths::new(&cfg.home).operation_lock() || !lock.is_exclusive() {
            return message("rollback requires the exclusive instance home operation lock");
        }
        let applied: Vec<(&dyn Step, Value)> = self
            .steps
            .iter()
            .filter_map(|step| {
                let state = step.state(cfg)?;
                if state.get("status").and_then(Value::as_str) == Some("applied") {
                    Some((step.as_ref(), state))
                } else {
                    None
                }
            })
            .collect();
        if applied.is_empty() {
            println!("[rollback] no applied steps for {}", cfg.name);
            return Ok(Vec::new());
        }
        if last && step_id.is_some() {
            return message("use either --last or --step, not both");
        }
        let selected: Vec<(&dyn Step, Value)> = if last {
            vec![applied[applied.len() - 1].clone()]
        } else if let Some(step_id) = step_id {
            let ids: Vec<&str> = applied.iter().map(|(step, _)| step.id()).collect();
            let Some(index) = ids.iter().position(|id| *id == step_id) else {
                return message(format!(
                    "step {step_id:?} is not applied; applied steps: {}",
                    ids.join(", ")
                ));
            };
            applied[index..].to_vec()
        } else {
            return message("specify --last or --step <id>");
        };
        if selected.last().map(|(step, _)| step.scope()) == Some("global")
            && applied.iter().any(|(step, _)| step.scope() != "global")
        {
            return message("instance steps are still applied; roll them back before global steps");
        }
        let mut done = Vec::new();
        for (step, state) in selected.into_iter().rev() {
            self.rollback_step(step, &state, force)?;
            done.push(step.id().to_string());
        }
        let mut fields = Map::new();
        fields.insert("instance".to_string(), json!(cfg.name));
        fields.insert("steps".to_string(), json!(done));
        self.journal.append("rollback_done", fields)?;
        println!("[rollback] rolled back: {}", done.join(", "));
        Ok(done)
    }

    // ---- public: status ------------------------------------------------
    pub fn status(&self) -> Result<Value> {
        let cfg = &self.cfg;
        let mut steps = Vec::new();
        for step in &self.steps {
            if !self.visible_step_ids().contains(&step.id().to_string()) {
                continue;
            }
            let (result, ctx) = self.check(step.as_ref(), "status")?;
            let state = step.state(cfg).unwrap_or(Value::Null);
            steps.push(json!({
                "id": step.id(),
                "title": step.title(),
                "scope": if step.scope() == "global" { "global" } else { "instance" },
                "ok": result.ok,
                "detail": result.detail,
                "state": state.get("status").and_then(Value::as_str).unwrap_or("new"),
                "log": state.get("log").cloned().unwrap_or(Value::Null),
            }));
            ctx.close();
        }
        let runtime = Runtime::new(cfg);
        let credentials = read_json(&cfg.credentials_path()).unwrap_or(Value::Null);
        let meta = load_instance_meta(&cfg.home, &cfg.name).unwrap_or(Value::Null);
        Ok(json!({
            "instance": cfg.name,
            "home": cfg.home.display().to_string(),
            "config": cfg.storage(),
            "instance_meta": meta,
            "steps": steps,
            "runtime": runtime.status(),
            "credentials_present": credentials.is_object() && !credentials.as_object().map(|map| map.is_empty()).unwrap_or(true),
            "ssh_key_present": cfg.ssh_key_path().exists(),
            "device": self.device_status()?,
            "paths": {
                "instance_dir": cfg.instance_dir().display().to_string(),
                "disk": cfg.disk_path().display().to_string(),
                "seed": cfg.seed_path().display().to_string(),
                "serial_log": cfg.serial_log_path().display().to_string(),
                "provision_serial_log": cfg.provision_serial_log().display().to_string(),
                "journal": StatePaths::new(&cfg.home).journal().display().to_string(),
            },
        }))
    }

    pub fn device_status(&self) -> Result<Value> {
        let cfg = &self.cfg;
        if !cfg.device_enabled() {
            return Ok(json!({"enabled": false}));
        }
        let report = read_json(&cfg.device_report_path()).unwrap_or(Value::Null);
        let mut groups = Map::new();
        for group in ["cpu", "smbios", "devices", "network", "guest"] {
            let state = self
                .steps
                .iter()
                .find(|step| step.id() == format!("device-{group}"))
                .and_then(|step| step.state(cfg))
                .unwrap_or(Value::Null);
            groups.insert(
                group.to_string(),
                json!({
                    "state": state.get("status").and_then(Value::as_str).unwrap_or("new"),
                    "fingerprint": cfg.device_fingerprint(Some(group))?,
                }),
            );
        }
        Ok(json!({
            "enabled": true,
            "profile_fingerprint": cfg.device_fingerprint(None)?,
            "groups": Value::Object(groups),
            "runtime_phase": if cfg.device_runtime_marker().exists() { "runtime" } else { "install" },
            "sealed": cfg.device_seal_path().exists(),
            "report": {
                "present": cfg.device_report_path().exists(),
                "status": report.get("status").cloned().unwrap_or(Value::Null),
                "acceptable": report.get("acceptable").cloned().unwrap_or(Value::Null),
                "errors": report.get("errors").cloned().unwrap_or(Value::Null),
                "warnings": report.get("warnings").cloned().unwrap_or(Value::Null),
                "generated_at": report.get("generated_at").cloned().unwrap_or(Value::Null),
                "path": cfg.device_report_path().display().to_string(),
            },
        }))
    }

    pub fn verify_device(
        &self,
        group: Option<&str>,
        boot: bool,
        keep_running: bool,
    ) -> Result<Value> {
        let lock = FileLock::acquire(&StatePaths::new(&self.cfg.home).operation_lock())?;
        self.verify_device_locked(&lock, group, boot, keep_running)
    }

    pub fn verify_device_locked(
        &self,
        lock: &FileLock,
        _group: Option<&str>,
        boot: bool,
        keep_running: bool,
    ) -> Result<Value> {
        let cfg = &self.cfg;
        if lock.path() != StatePaths::new(&cfg.home).operation_lock() || !lock.is_exclusive() {
            return message(
                "device verification requires the exclusive instance home operation lock",
            );
        }
        if !cfg.device_enabled() {
            return message("device is not enabled for this instance");
        }
        let runtime = Runtime::new(cfg);
        let started = std::cell::Cell::new(false);
        let log_path = cfg
            .instance_logs_dir()
            .join(format!("device-verify-{}.log", localstamp()));
        ensure_dir(&cfg.instance_logs_dir())?;
        let file = cabin_diagnostics::open_capture(&log_path, cabin_diagnostics::Capture::Append)?;
        let log = Arc::new(Mutex::new(file));
        let result = (|| -> Result<Value> {
            if !runtime.running() {
                if !boot {
                    let report = read_json(&cfg.device_report_path()).unwrap_or(Value::Null);
                    if report.is_null()
                        || report.as_object().map(|map| map.is_empty()).unwrap_or(true)
                    {
                        return message("no stored device report; run without --no-boot");
                    }
                    return Ok(report);
                }
                if !cfg.provision_marker_path().exists() {
                    return message(format!(
                        "instance {:?} is not provisioned; run 'vmctl apply --name {}' first",
                        cfg.name, cfg.name
                    ));
                }
                let phase = if cfg.device_runtime_marker().exists() {
                    "runtime"
                } else {
                    "install"
                };
                crate::util::log_line(
                    &log,
                    &format!(
                        "# device verify instance={} phase={phase} at={}\n",
                        cfg.name,
                        now_millis()
                    ),
                );
                runtime.start(
                    &log,
                    Boot {
                        serial_log: Some(
                            &cfg.instance_dir()
                                .join(format!("serial-verify-{}.log", localstamp())),
                        ),
                        phase: Some(phase),
                        ..Boot::new(Duration::from_secs(300))
                    },
                )?;
                started.set(true);
            }
            runtime.wait_for_ssh(&log, Duration::from_secs(180))?;
            self.ensure_helper(&runtime)?;
            let (rc, out) = runtime.ssh_capture(
                &format!("sudo -n {} device-probe", crate::agentd::HELPER_BIN),
                Duration::from_secs(240),
            );
            if rc != 0 {
                let tail: String = out
                    .chars()
                    .rev()
                    .take(4000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                return message(format!("guest probe failed (rc={rc}):\n{tail}"));
            }
            let raw: Value = serde_json::from_str(&out).map_err(|error| {
                crate::Error::Message(format!("guest probe returned invalid JSON: {error}"))
            })?;
            let report = device_verify::evaluate(&raw, &cfg.device_resolved()?, cfg);
            atomic_write_json(&cfg.device_raw_path(), &raw, 0o600)?;
            atomic_write_json(&cfg.device_report_path(), &report, 0o600)?;
            Ok(report)
        })();
        if started.get() && !keep_running && runtime.alive() {
            runtime.stop(&log, Duration::from_secs(90));
        }
        result
    }

    pub fn reconfigure(
        &self,
        user: Option<&str>,
        hostname: Option<&str>,
        timezone: Option<&str>,
        locale: Option<&str>,
    ) -> Result<Value> {
        let lock = FileLock::acquire(&StatePaths::new(&self.cfg.home).operation_lock())?;
        self.reconfigure_locked(&lock, user, hostname, timezone, locale)
    }

    /// Reconfigure while the caller holds this home's exclusive operation lock.
    pub fn reconfigure_locked(
        &self,
        lock: &FileLock,
        user: Option<&str>,
        hostname: Option<&str>,
        timezone: Option<&str>,
        locale: Option<&str>,
    ) -> Result<Value> {
        self.require_exclusive_operation_lock(lock, "reconfigure")?;
        let cfg = &self.cfg;
        if user.is_none() && hostname.is_none() && timezone.is_none() && locale.is_none() {
            return message(
                "reconfigure needs at least one of --user, --hostname, --timezone, --locale",
            );
        }
        if let Some(user) = user
            && !crate::config::validation_regex(r"^[a-z_][a-z0-9_-]{0,31}$")?.is_match(user)
        {
            return message("--user must be a valid lowercase Linux username");
        }
        if let Some(hostname) = hostname
            && !valid_hostname(hostname)
        {
            return message("--hostname must be a valid RFC1123 hostname");
        }
        let old_identity = cfg.current_identity();
        let old_user = cfg.current_username();
        let old_hostname = cfg.current_hostname();
        let old_timezone = cfg.current_timezone();
        let old_locale = cfg.current_locale();
        let new_user = user.unwrap_or(&old_user).to_string();
        let new_hostname = hostname.unwrap_or(&old_hostname).to_string();
        let new_timezone = timezone.unwrap_or(&old_timezone).to_string();
        let new_locale = locale.unwrap_or(&old_locale).to_string();
        if let Some(user) = user
            && user != old_user
            && user == "root"
        {
            return message("refusing to rename the login account to root");
        }
        let runtime = Runtime::new(cfg);
        let was_running = runtime.running();
        let old_report_bytes = std::fs::read(cfg.device_report_path()).ok();
        let old_raw_bytes = std::fs::read(cfg.device_raw_path()).ok();
        ensure_dir(&cfg.instance_logs_dir())?;
        let log_path = cfg
            .instance_logs_dir()
            .join(format!("reconfigure-{}.log", localstamp()));
        let snap_ctx = StepContext::new(cfg, "reconfigure", "prepare")?;
        let mut identity_written = false;
        let result = (|| -> Result<Value> {
            ensure_stopped(cfg, &snap_ctx)?;
            let phase = if cfg.device_runtime_marker().exists() {
                "runtime"
            } else {
                "install"
            };
            let mut log = cabin_diagnostics::open_capture(&log_path, cabin_diagnostics::Capture::Append)?;
            writeln!(
                log,
                "# reconfigure {old_user} -> {new_user}, {old_hostname} -> {new_hostname}"
            )?;
            let log_arc = Arc::new(Mutex::new(log));
            runtime.start(
                &log_arc,
                Boot {
                    phase: Some(phase),
                    ..Boot::new(Duration::from_secs(300))
                },
            )?;
            let changes = ReconfigureChanges {
                old_user: &old_user,
                new_user: &new_user,
                old_host: &old_hostname,
                new_host: &new_hostname,
                old_timezone: &old_timezone,
                new_timezone: &new_timezone,
                old_locale: &old_locale,
                new_locale: &new_locale,
            };
            let script = self.reconfigure_script(&changes)?;
            let (rc, out) = runtime.ssh_script(&script, Duration::from_secs(180));
            let tail: String = out
                .chars()
                .rev()
                .take(8000)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            crate::util::log_line(&log_arc, &tail);
            if rc != 0 {
                return message(format!("guest reconfigure failed (rc={rc})"));
            }
            let mut identity = old_identity.as_object().cloned().unwrap_or_default();
            identity.insert("username".to_string(), json!(new_user));
            identity.insert("hostname".to_string(), json!(new_hostname));
            identity.insert("timezone".to_string(), json!(new_timezone));
            identity.insert("locale".to_string(), json!(new_locale));
            identity.insert("updated_at".to_string(), json!(now_millis()));
            identity.insert(
                "previous".to_string(),
                json!({
                    "username": old_user,
                    "hostname": old_hostname,
                    "timezone": old_timezone,
                    "locale": old_locale,
                }),
            );
            atomic_write_json(&cfg.identity_path(), &Value::Object(identity), 0o600)?;
            identity_written = true;
            let (verify_rc, verify_out) =
                runtime.ssh_capture("id -un && sudo -n true && echo ok", Duration::from_secs(20));
            if verify_rc != 0 || !verify_out.contains("ok") {
                return message(format!(
                    "new identity {new_user:?} could not log in or use sudo: {}",
                    verify_out.trim()
                ));
            }
            if old_user != new_user {
                let mut removal_ok = false;
                let mut removal_out = String::new();
                for _ in 0..15 {
                    let (removal_rc, out) = runtime.ssh_capture(
                        &format!(
                            "sudo -n sh -c 'userdel -r {} 2>&1 || userdel {} 2>&1; rc=$?; rm -rf /home/{}; exit $rc'",
                            crate::util::shell_quote(&old_user),
                            crate::util::shell_quote(&old_user),
                            crate::util::shell_quote(&old_user)
                        ),
                        Duration::from_secs(60),
                    );
                    removal_out = out;
                    if removal_rc == 0 {
                        removal_ok = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
                if !removal_ok {
                    return message(format!(
                        "could not remove the old account {old_user:?}: {}",
                        removal_out.trim()
                    ));
                }
            }
            if cfg.device_enabled() {
                self.ensure_helper(&runtime)?;
                let (probe_rc, probe_out) = runtime.ssh_capture(
                    &format!("sudo -n {} device-probe", crate::agentd::HELPER_BIN),
                    Duration::from_secs(240),
                );
                if probe_rc != 0 {
                    let tail: String = probe_out
                        .chars()
                        .rev()
                        .take(2000)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                    return message(format!(
                        "post-reconfigure device probe failed (rc={probe_rc}): {tail}"
                    ));
                }
                let probe_raw: Value = serde_json::from_str(&probe_out).map_err(|error| {
                    crate::Error::Message(format!(
                        "post-reconfigure probe returned invalid JSON: {error}"
                    ))
                })?;
                let probe_report =
                    device_verify::evaluate(&probe_raw, &cfg.device_resolved()?, cfg);
                atomic_write_json(&cfg.device_raw_path(), &probe_raw, 0o600)?;
                atomic_write_json(&cfg.device_report_path(), &probe_report, 0o600)?;
                if !probe_report
                    .get("acceptable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return message(format!(
                        "post-reconfigure device report is not acceptable (errors={}, warnings={})",
                        probe_report
                            .get("errors")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                        probe_report
                            .get("warnings")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                    ));
                }
            }
            runtime.ssh_capture("sudo -n shutdown -P now", Duration::from_secs(20));
            let deadline = Instant::now() + Duration::from_secs(120);
            while Instant::now() < deadline && runtime.alive() {
                std::thread::sleep(Duration::from_secs(1));
            }
            if runtime.alive() {
                runtime.stop(&log_arc, Duration::from_secs(30));
            }
            runtime.cleanup_stale();
            let mut credentials = read_json(&cfg.credentials_path()).unwrap_or(Value::Null);
            if credentials.is_object() {
                if let Some(map) = credentials.as_object_mut() {
                    map.insert("username".to_string(), json!(new_user));
                    map.insert("hostname".to_string(), json!(new_hostname));
                }
                atomic_write_json(&cfg.credentials_path(), &credentials, 0o600)?;
            }
            let mut fields = Map::new();
            fields.insert("instance".to_string(), json!(cfg.name));
            fields.insert(
                "old".to_string(),
                json!({"username": old_user, "hostname": old_hostname, "timezone": old_timezone, "locale": old_locale}),
            );
            fields.insert(
                "new".to_string(),
                json!({"username": new_user, "hostname": new_hostname, "timezone": new_timezone, "locale": new_locale}),
            );
            fields.insert("log".to_string(), json!(log_path.display().to_string()));
            self.journal.append("reconfigure", fields)?;
            Ok(json!({
                "username": new_user,
                "hostname": new_hostname,
                "timezone": new_timezone,
                "locale": new_locale,
                "log": log_path.display().to_string(),
            }))
        })();
        let result = match result {
            Ok(value) => value,
            Err(error) => {
                let rollback = (|| -> Result<()> {
                    // Only host-side records are restored; the guest keeps
                    // whatever the failed script changed (no disk snapshot).
                    ensure_stopped(cfg, &snap_ctx)?;
                    if identity_written {
                        if old_identity.is_object()
                            && !old_identity
                                .as_object()
                                .map(|map| map.is_empty())
                                .unwrap_or(true)
                        {
                            atomic_write_json(&cfg.identity_path(), &old_identity, 0o600)?;
                        } else {
                            let _ = std::fs::remove_file(cfg.identity_path());
                        }
                    }
                    match &old_report_bytes {
                        Some(bytes) => atomic_write_bytes(&cfg.device_report_path(), bytes, 0o600)?,
                        None => {
                            let _ = std::fs::remove_file(cfg.device_report_path());
                        }
                    }
                    match &old_raw_bytes {
                        Some(bytes) => atomic_write_bytes(&cfg.device_raw_path(), bytes, 0o600)?,
                        None => {
                            let _ = std::fs::remove_file(cfg.device_raw_path());
                        }
                    }
                    if was_running {
                        let log = Arc::new(Mutex::new(
                            cabin_diagnostics::open_capture(&log_path, cabin_diagnostics::Capture::Append)?,
                        ));
                        let _ =
                            runtime.start(&log, Boot::new(Duration::from_secs(180)));
                    }
                    Ok(())
                })();
                if let Err(rollback_error) = rollback {
                    snap_ctx.log(&format!("reconfigure rollback failed: {rollback_error}"));
                }
                snap_ctx.close();
                return message(format!(
                    "reconfigure failed: {error} (log: {})",
                    log_path.display()
                ));
            }
        };
        snap_ctx.close();
        if was_running {
            let _ = self.start_vm_locked(lock);
        }
        Ok(result)
    }

    fn reconfigure_script(&self, changes: &ReconfigureChanges<'_>) -> Result<String> {
        let ReconfigureChanges {
            old_user,
            new_user,
            old_host,
            new_host,
            old_timezone: old_tz,
            new_timezone: new_tz,
            old_locale,
            new_locale,
        } = *changes;
        let q = crate::util::shell_quote;
        let mut lines: Vec<String> = vec![
            "set -euo pipefail".to_string(),
            "echo \"[reconfigure] starting\"".to_string(),
        ];
        if old_user != new_user {
            lines.extend([
                format!(
                    "if id -u {} >/dev/null 2>&1; then echo \"user {new_user} exists\"; exit 20; fi",
                    q(new_user)
                ),
                format!(
                    "useradd -m -s /bin/bash -G adm,sudo,audio -c {} {}",
                    q(new_user),
                    q(new_user)
                ),
                format!(
                    "usermod -p \"$(getent shadow {} | cut -d: -f2)\" {}",
                    q(old_user),
                    q(new_user)
                ),
                format!(
                    "cp -a {} {} 2>/dev/null || true",
                    q(&format!("/home/{old_user}/.")),
                    q(&format!("/home/{new_user}/"))
                ),
                format!(
                    "chown -R {}:{} {} || true",
                    q(new_user),
                    q(new_user),
                    q(&format!("/home/{new_user}"))
                ),
                format!("chmod 700 {} 2>/dev/null || true", q(&format!("/home/{new_user}/.ssh"))),
                format!(
                    "chmod 600 {} 2>/dev/null || true",
                    q(&format!("/home/{new_user}/.ssh/authorized_keys"))
                ),
                format!(
                    "printf '%s ALL=(ALL) NOPASSWD:ALL\\n' {} > /etc/sudoers.d/90-local-admin",
                    q(new_user)
                ),
                "chmod 440 /etc/sudoers.d/90-local-admin".to_string(),
                "visudo -cf /etc/sudoers.d/90-local-admin >/dev/null".to_string(),
                format!("echo '[reconfigure] new user {new_user} prepared'"),
            ]);
        }
        if old_host != new_host {
            lines.extend([
                format!("hostnamectl set-hostname {}", q(new_host)),
                format!("printf '%s\\n' {} > /etc/hostname", q(new_host)),
                "sed -i '/^127\\.0\\.1\\.1[[:space:]]/d' /etc/hosts || true".to_string(),
                format!("printf '127.0.1.1\\t%s\\n' {} >> /etc/hosts", q(new_host)),
                format!("echo '[reconfigure] hostname set to {new_host}'"),
            ]);
        }
        if old_tz != new_tz {
            lines.extend([
                format!("timedatectl set-timezone {}", q(new_tz)),
                format!("printf '%s\\n' {} > /etc/timezone", q(new_tz)),
                format!("echo '[reconfigure] timezone set to {new_tz}'"),
            ]);
        }
        if old_locale != new_locale {
            lines.extend([
                format!(
                    "grep -q '^{new_locale} ' /etc/locale.gen 2>/dev/null || echo {} >> /etc/locale.gen",
                    q(&format!("{new_locale} UTF-8"))
                ),
                "locale-gen".to_string(),
                format!(
                    "update-locale LANG={} LC_ALL={} LANGUAGE={}",
                    q(new_locale),
                    q(new_locale),
                    q(new_locale)
                ),
                format!(
                    "printf 'LANG=%s\\nLC_ALL=%s\\nLANGUAGE=%s\\n' {} {} {} > /etc/default/locale",
                    q(new_locale),
                    q(new_locale),
                    q(new_locale)
                ),
                format!("echo '[reconfigure] locale set to {new_locale}'"),
            ]);
        }
        if old_host != new_host
            && self.cfg.device_enabled()
            && let Ok(resolved) = self.cfg.device_resolved()
            && let Ok(plan) = device_render::netplan_document(&resolved, Some(new_host))
        {
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD
                .encode((crate::py_json::dumps_pretty(&plan) + "\n").as_bytes());
            lines.extend([
                format!(
                    "printf '%s' {} | base64 -d > /etc/netplan/50-vmctl.yaml",
                    q(&encoded)
                ),
                "chmod 600 /etc/netplan/50-vmctl.yaml".to_string(),
            ]);
        }
        lines.push("echo \"[reconfigure] complete\"".to_string());
        Ok(lines.join("\n") + "\n")
    }

    // ---- public: lifecycle --------------------------------------------
    pub fn start_vm(&self) -> Result<Value> {
        let lock = FileLock::acquire(&StatePaths::new(&self.cfg.home).operation_lock())?;
        self.start_vm_locked(&lock)
    }

    /// Start while the caller holds this home's exclusive operation lock.
    pub fn start_vm_locked(&self, lock: &FileLock) -> Result<Value> {
        self.require_exclusive_operation_lock(lock, "start")?;
        let cfg = &self.cfg;
        let runtime = Runtime::new(cfg);
        if runtime.running() {
            if proxy::should_manage(cfg.proxy.as_deref()) && !proxy::restricted_runtime(&runtime) {
                return message("Restart the instance to enable the restricted network");
            }
            let proxy_was_running = proxy::status(&cfg.home)?.running;
            if proxy::should_manage(cfg.proxy.as_deref()) {
                proxy::ensure_started(&cfg.home)?;
            }
            let prepared = crate::guest_dns::ensure(cfg, &runtime)
                .and_then(|_| crate::workspace::mount_mappings(cfg, &runtime));
            if let Err(error) = prepared {
                let mut log_file = cabin_diagnostics::open_capture(&cfg.instance_logs_dir().join("start-rollback.log"), cabin_diagnostics::Capture::Append)?;
                    writeln!(
                    log_file,
                    "# start rollback instance={} at={}\n{}",
                    cfg.name,
                    now_millis(),
                    error
                )?;
                runtime.stop(&Arc::new(Mutex::new(log_file)), Duration::from_secs(45));
                if proxy::should_manage(cfg.proxy.as_deref()) && !proxy_was_running {
                    let _ = proxy::stop(&cfg.home, false);
                }
                return Err(error);
            }
            println!(
                "[start] {} is already running (pid {:?})",
                cfg.name,
                runtime.pid()
            );
            return Ok(runtime.status());
        }
        if !cfg.provision_marker_path().exists() {
            return message(format!(
                "instance {:?} is not provisioned; run 'vmctl apply --name {}' first",
                cfg.name, cfg.name
            ));
        }
        ensure_dir(&cfg.instance_logs_dir())?;
        let log_path = cfg
            .instance_logs_dir()
            .join(format!("start-{}.log", localstamp()));
        let mut log_file = cabin_diagnostics::open_capture(&log_path, cabin_diagnostics::Capture::Append)?;
        writeln!(log_file, "# start instance={} at={}", cfg.name, now_millis())?;
        let log = Arc::new(Mutex::new(log_file));
        self.start_stopped(&runtime, &log, &log_path, None)?;
        println!("[start] ssh: {}", self.ssh_command()?);
        Ok(runtime.status())
    }

    pub(crate) fn start_for_app_locked(
        &self,
        lock: &FileLock,
        runtime: &Runtime,
        attempt: &Attempt,
    ) -> Result<()> {
        self.require_exclusive_operation_lock(lock, "start")?;
        let log = attempt
            .log()
            .ok_or_else(|| crate::Error::Message("Startup log is unavailable".into()))?;
        let log_path = attempt
            .log_path()
            .ok_or_else(|| crate::Error::Message("Startup log path is unavailable".into()))?;
        attempt.follow_serial(self.cfg.serial_log_path(), false);
        self.start_stopped(runtime, log, log_path, Some(attempt))
    }

    fn start_stopped(
        &self,
        runtime: &Runtime,
        log: &Arc<Mutex<std::fs::File>>,
        log_path: &std::path::Path,
        attempt: Option<&Attempt>,
    ) -> Result<()> {
        let cfg = &self.cfg;
        if let Some(attempt) = attempt {
            attempt.begin(Phase::Qemu);
        }
        if !cfg.provision_marker_path().exists() {
            return message(format!(
                "instance {:?} is not provisioned; run 'vmctl apply --name {}' first",
                cfg.name, cfg.name
            ));
        }
        // Refuse to overlap an existing QEMU even if its monitor is unreachable.
        if runtime.alive() {
            return message("The previous QEMU process has not exited");
        }
        let manage_proxy = proxy::should_manage(cfg.proxy.as_deref());
        let proxy_was_running = manage_proxy && proxy::status(&cfg.home)?.running;
        let started = (|| -> Result<()> {
            if manage_proxy {
                proxy::ensure_started(&cfg.home)?;
            }
            runtime.start_stopped(log, Boot::new(Duration::from_secs(180)), attempt)?;
            if let Some(attempt) = attempt {
                attempt.begin(Phase::Guest);
            }
            // Native guest helpers can be needed by DNS, desktop and browser
            // operations before the daemon handshake deploys its user service.
            self.ensure_helper(runtime)?;
            if let Err(error) = crate::resources::verify_boot(cfg, runtime) {
                // Disk growth is forward-only. Leave the VM usable and retain
                // the pending request for another attempt after a clean stop.
                let message = error.to_string();
                crate::util::log_line(log, &format!("[machine resources] {message}"));
                if let Some(attempt) = attempt {
                    attempt.note(&message);
                }
                if let Err(error) = crate::resources::record_notice(cfg, &message) {
                    crate::util::log_line(log, &format!("[machine resources] Could not save the notice: {error}"));
                }
            }
            std::thread::scope(|scope| {
                let dns = std::thread::Builder::new()
                    .name("guest-dns".into())
                    .spawn_scoped(scope, || crate::guest_dns::ensure(cfg, runtime))?;
                let folders = crate::workspace::mount_mappings(cfg, runtime);
                dns.join()
                    .map_err(|_| crate::Error::Message("Guest DNS worker stopped unexpectedly".into()))?
                    .and(folders)
            })?;
            if let Some(attempt) = attempt {
                attempt.complete(Phase::Guest, "Guest DNS and shared folders are ready");
            }
            Ok(())
        })();
        if let Err(error) = started {
            crate::util::log_line(log, &format!("[start rollback] {error}"));
            if runtime.alive() {
                runtime.stop(log, Duration::from_secs(45));
            }
            if manage_proxy && !proxy_was_running {
                let _ = proxy::stop(&cfg.home, false);
            }
            return Err(error);
        }
        // IP enumeration is a CLI diagnostic, not part of app readiness.
        let ips = if attempt.is_none() {
            runtime.guest_ips()
        } else {
            Default::default()
        };
        let mut fields = Map::new();
        fields.insert("instance".to_string(), json!(cfg.name));
        fields.insert("pid".to_string(), json!(runtime.pid()));
        fields.insert("ips".to_string(), json!(ips));
        fields.insert("log".to_string(), json!(log_path.display().to_string()));
        self.journal.append("vm_start", fields)?;
        println!("[start] {} running, pid {:?}", cfg.name, runtime.pid());
        if !ips.is_empty() {
            println!("[start] guest ips: {ips:?}");
        }
        Ok(())
    }

    pub fn stop_vm(&self) -> Result<()> {
        let lock = FileLock::acquire(&StatePaths::new(&self.cfg.home).operation_lock())?;
        self.stop_vm_locked(&lock)
    }

    /// Stop while the caller holds this home's exclusive operation lock.
    pub fn stop_vm_locked(&self, lock: &FileLock) -> Result<()> {
        self.require_exclusive_operation_lock(lock, "stop")?;
        let cfg = &self.cfg;
        ensure_dir(&cfg.instance_logs_dir())?;
        let log_path = cfg
            .instance_logs_dir()
            .join(format!("stop-{}.log", localstamp()));
        let mut log_file = cabin_diagnostics::open_capture(&log_path, cabin_diagnostics::Capture::Append)?;
        writeln!(log_file, "# stop instance={} at={}", cfg.name, now_millis())?;
        let log = Arc::new(Mutex::new(log_file));
        let runtime = Runtime::new(cfg);
        runtime.stop(&log, Duration::from_secs(90));
        if runtime.alive() {
            return message(format!("The machine could not be stopped. See {}", log_path.display()));
        }
        let mut fields = Map::new();
        fields.insert("instance".to_string(), json!(cfg.name));
        fields.insert("log".to_string(), json!(log_path.display().to_string()));
        self.journal.append("vm_stop", fields)?;
        println!("[stop] {} stopped", cfg.name);
        Ok(())
    }

    pub fn destroy(&self, all_resources: bool) -> Result<()> {
        let lock = FileLock::acquire(&StatePaths::new(&self.cfg.home).operation_lock())?;
        self.destroy_locked(&lock, all_resources)
    }

    /// Destroy while the caller holds this home's exclusive operation lock.
    pub fn destroy_locked(&self, lock: &FileLock, all_resources: bool) -> Result<()> {
        self.require_exclusive_operation_lock(lock, "destroy")?;
        let cfg = &self.cfg;
        let runtime = Runtime::new(cfg);
        if runtime.alive() {
            ensure_dir(&cfg.instance_logs_dir())?;
            let log_path = cfg
                .instance_logs_dir()
                .join(format!("destroy-{}.log", localstamp()));
            let log = Arc::new(Mutex::new(
                cabin_diagnostics::open_capture(&log_path, cabin_diagnostics::Capture::Append)?,
            ));
            runtime.stop(&log, Duration::from_secs(60));
            if runtime.alive() {
                return message("The machine could not be stopped; its data was kept");
            }
        }
        let applied: Vec<(&dyn Step, Value)> = self
            .steps
            .iter()
            .filter_map(|step| {
                if step.scope() == "global" {
                    return None;
                }
                let state = step.state(cfg)?;
                if state.get("status").and_then(Value::as_str) == Some("applied") {
                    Some((step.as_ref(), state))
                } else {
                    None
                }
            })
            .collect();
        for (step, state) in applied.into_iter().rev() {
            self.rollback_step(step, &state, true)?;
        }
        if cfg.instance_dir().exists() {
            println!("[destroy] removing {}", cfg.instance_dir().display());
            crate::util::remove_path(&cfg.instance_dir())?;
        }
        if all_resources {
            for step_id in ["image", "qemu"] {
                if let Some(step) = self.steps.iter().find(|step| step.id() == step_id)
                    && let Some(state) = step.state(cfg)
                    && state.get("status").and_then(Value::as_str) == Some("applied")
                {
                    self.rollback_step(step.as_ref(), &state, true)?;
                }
            }
        }
        let mut fields = Map::new();
        fields.insert("instance".to_string(), json!(cfg.name));
        fields.insert("all_resources".to_string(), json!(all_resources));
        self.journal.append("destroy", fields)?;
        println!("[destroy] instance {} removed", cfg.name);
        Ok(())
    }

    pub fn ssh_command(&self) -> Result<String> {
        let cfg = &self.cfg;
        let (host, port) = if cfg.device_enabled() {
            device_render::ssh_endpoint(cfg, None)?
        } else {
            ("127.0.0.1".to_string(), cfg.ssh_connect_port())
        };
        Ok(format!(
            "ssh -i {} -p {port} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null {}@{host}",
            cfg.ssh_key_path().display(),
            cfg.current_username()
        ))
    }

    pub fn logs(&self, step: Option<&str>, serial: bool) -> Vec<PathBuf> {
        let cfg = &self.cfg;
        let mut files: Vec<PathBuf> = Vec::new();
        if serial {
            for path in [cfg.serial_log_path(), cfg.provision_serial_log(), cfg.instance_file("serial.log"), StatePaths::new(&cfg.home).instance(&cfg.name).provision_serial()] {
                if path.exists() {
                    files.push(path);
                }
            }
        } else if let Some(step) = step {
            if let Ok(entries) = std::fs::read_dir(cfg.instance_logs_dir()) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .map(|name| name.starts_with(&format!("{step}-")) && name.ends_with(".log"))
                        .unwrap_or(false)
                    {
                        files.push(path);
                    }
                }
            }
        } else {
            let events = cfg.logs_dir().join(cabin_diagnostics::EVENT_FILE);
            if std::fs::symlink_metadata(&events).is_ok_and(|meta| meta.is_file()) {
                files.push(events);
            }
            for directory in [cfg.instance_logs_dir(), cfg.logs_dir().join("global")] {
                if let Ok(entries) = std::fs::read_dir(directory) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|value| value.to_str()) == Some("log") {
                            files.push(path);
                        }
                    }
                }
            }
        }
        files.sort_by(|a, b| {
            let left = std::fs::metadata(a)
                .and_then(|metadata| metadata.modified())
                .ok();
            let right = std::fs::metadata(b)
                .and_then(|metadata| metadata.modified())
                .ok();
            right.cmp(&left)
        });
        files
    }
}

fn valid_hostname(text: &str) -> bool {
    if text.is_empty() || text.len() > 63 {
        return false;
    }
    let bytes = text.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    if !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit() {
        return false;
    }
    text.chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_locked_operations_require_the_correct_exclusive_lock() {
        let home = std::env::temp_dir().join(format!(
            "vmctl-lifecycle-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let cfg = crate::config::build_config(
            &serde_json::json!({
                "name": "test", "home": home, "network_mode": "user",
                "ssh_port": listener.local_addr().unwrap().port(),
                "device": {"enabled": false}
            }),
            &crate::config::default_project_root(),
        )
        .unwrap();
        let runner = Runner::new(cfg).unwrap();
        let lock_path = StatePaths::new(&home).operation_lock();

        let wrong = FileLock::acquire(&home.join("wrong.lock")).unwrap();
        assert!(runner.apply_locked(&wrong).is_err());
        assert!(runner.start_vm_locked(&wrong).is_err());
        assert!(
            runner
                .reconfigure_locked(&wrong, None, None, None, None)
                .is_err()
        );
        assert!(runner.stop_vm_locked(&wrong).is_err());
        assert!(runner.destroy_locked(&wrong, false).is_err());
        drop(wrong);
        let shared = FileLock::acquire_shared(&lock_path).unwrap();
        assert!(runner.apply_locked(&shared).is_err());
        assert!(runner.start_vm_locked(&shared).is_err());
        assert!(
            runner
                .reconfigure_locked(&shared, None, None, None, None)
                .is_err()
        );
        assert!(runner.stop_vm_locked(&shared).is_err());
        assert!(runner.destroy_locked(&shared, false).is_err());
        drop(shared);

        let exclusive = FileLock::acquire(&lock_path).unwrap();
        let error = runner.apply_locked(&exclusive).unwrap_err().to_string();
        assert!(error.contains("already in use"), "{error}");
        runner.destroy_locked(&exclusive, false).unwrap();
        assert!(!runner.cfg.instance_dir().exists());
        drop(exclusive);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn device_verification_excludes_other_instance_operations() {
        let home = std::env::temp_dir().join(format!(
            "vmctl-device-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cfg = crate::config::build_config(
            &serde_json::json!({"name": "test", "home": home}),
            &crate::config::default_project_root(),
        )
        .unwrap();
        let runner = Runner::new(cfg).unwrap();
        let lock_path = StatePaths::new(&home).operation_lock();
        let shared = FileLock::acquire_shared(&lock_path).unwrap();
        assert!(
            runner
                .verify_device_locked(&shared, None, false, false)
                .is_err()
        );
        assert!(
            runner
                .verify_device(None, false, false)
                .unwrap_err()
                .to_string()
                .contains("another vmctl operation")
        );
        drop(shared);
        assert!(
            runner
                .verify_device(None, false, false)
                .unwrap_err()
                .to_string()
                .contains("device is not enabled")
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn rollback_locked_requires_the_correct_exclusive_lock() {
        let home = std::env::temp_dir().join(format!(
            "vmctl-rollback-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cfg = crate::config::build_config(
            &serde_json::json!({"name": "test", "home": home}),
            &crate::config::default_project_root(),
        )
        .unwrap();
        let runner = Runner::new(cfg).unwrap();
        let lock_path = StatePaths::new(&home).operation_lock();
        let wrong = FileLock::acquire(&home.join("wrong.lock")).unwrap();
        assert!(runner.rollback_locked(&wrong, None, true, false).is_err());
        drop(wrong);
        let shared = FileLock::acquire_shared(&lock_path).unwrap();
        assert!(runner.rollback_locked(&shared, None, true, false).is_err());
        drop(shared);
        let exclusive = FileLock::acquire(&lock_path).unwrap();
        assert!(
            runner
                .rollback_locked(&exclusive, None, true, false)
                .unwrap()
                .is_empty()
        );
        drop(exclusive);
        let _ = std::fs::remove_dir_all(home);
    }
}
