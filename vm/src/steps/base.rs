//! Step framework: each step has check(), apply(), rollback() and a log.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::Result;
use crate::config::Config;
use crate::journal::{load_state, save_state};
use crate::util::{
    RunOptions, capture_env, ensure_dir, localstamp, message, now_millis, remove_path, run,
    sha256_file,
};
use crate::vm::{Runtime, resolve_qemu_img};

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub ok: bool,
    pub detail: String,
}

impl CheckResult {
    pub fn ready(detail: impl Into<String>) -> Self {
        Self {
            ok: true,
            detail: detail.into(),
        }
    }

    pub fn pending(detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Artifact {
    pub path: String,
    pub kind: String,
    pub sha256: Option<String>,
    pub size: Option<u64>,
}

impl Artifact {
    pub fn as_value(&self) -> Value {
        json!({
            "path": self.path,
            "kind": self.kind,
            "sha256": self.sha256,
            "size": self.size,
        })
    }
}

pub struct StepContext {
    pub cfg: Config,
    pub step_id: String,
    pub action: String,
    pub log_path: PathBuf,
    pub log_file: Arc<Mutex<File>>,
    pub artifacts: Vec<Artifact>,
    pub meta: Map<String, Value>,
    /// Host path of the Linux guest binary, provided by the VM runner.
    pub agentd_binary: Option<PathBuf>,
    /// Unix milliseconds.
    pub started_at: i64,
    started: Instant,
}

impl StepContext {
    pub fn new(cfg: &Config, step_id: &str, action: &str) -> Result<Self> {
        let log_dir = if matches!(step_id, "qemu" | "image") {
            cfg.logs_dir().join("global")
        } else {
            cfg.instance_logs_dir()
        };
        ensure_dir(&log_dir)?;
        let log_path = log_dir.join(format!("{step_id}-{action}-{}.log", localstamp()));
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let started_at = now_millis();
        writeln!(
            file,
            "# step={step_id} action={action} instance={} at={started_at}",
            cfg.name
        )?;
        file.flush()?;
        Ok(Self {
            cfg: cfg.clone(),
            step_id: step_id.to_string(),
            action: action.to_string(),
            log_path,
            log_file: Arc::new(Mutex::new(file)),
            artifacts: Vec::new(),
            meta: Map::new(),
            agentd_binary: None,
            started_at,
            started: Instant::now(),
        })
    }

    pub fn helper_binary(&self) -> Result<&Path> {
        let path = self.agentd_binary.as_deref().ok_or_else(|| {
            crate::Error::Message("Guest cabind binary path is unavailable. Set CABIND_BIN to the Linux ARM64 cabind binary.".into())
        })?;
        crate::agentd::require_binary(path)
    }

    pub fn log(&self, text: &str) {
        if let Ok(mut file) = self.log_file.lock() {
            let _ = writeln!(file, "{text}");
            let _ = file.flush();
        }
        println!("[{}] {text}", self.step_id);
    }

    pub fn close(&self) {
        if let Ok(mut file) = self.log_file.lock() {
            let _ = file.flush();
        }
    }

    pub fn elapsed(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn run(&self, argv: &[String], timeout: std::time::Duration, check: bool) -> Result<i32> {
        run(
            argv,
            &self.log_file,
            RunOptions {
                timeout,
                check,
                ..Default::default()
            },
        )
    }

    pub fn track(&mut self, path: &Path, kind: &str, hash_file: bool) -> Result<Artifact> {
        if !path.exists() {
            return message(format!("cannot track missing artifact: {}", path.display()));
        }
        let artifact = if path.is_dir() {
            Artifact {
                path: path.display().to_string(),
                kind: "dir".to_string(),
                sha256: None,
                size: None,
            }
        } else {
            Artifact {
                path: path.display().to_string(),
                kind: kind.to_string(),
                sha256: if hash_file {
                    Some(sha256_file(path)?)
                } else {
                    None
                },
                size: std::fs::metadata(path).ok().map(|metadata| metadata.len()),
            }
        };
        if let Some(existing) = self
            .artifacts
            .iter_mut()
            .find(|item| item.path == artifact.path)
        {
            *existing = artifact.clone();
        } else {
            self.artifacts.push(artifact.clone());
        }
        Ok(artifact)
    }

    pub fn track_file(&mut self, path: &Path) -> Result<Artifact> {
        self.track(path, "file", true)
    }

    pub fn set_meta(&mut self, key: &str, value: Value) {
        self.meta.insert(key.to_string(), value);
    }
}

pub fn ensure_stopped(cfg: &Config, ctx: &StepContext) -> Result<()> {
    let runtime = Runtime::new(cfg);
    if runtime.alive() {
        ctx.log("stopping running VM before rollback/apply");
        runtime.stop(&ctx.log_file, std::time::Duration::from_secs(60));
        if runtime.alive() {
            return message("The machine could not be stopped; its files were kept");
        }
    } else {
        runtime.cleanup_stale();
    }
    Ok(())
}

fn qemu_img_capture(
    cfg: &Config,
    disk: &Path,
    args: &[&str],
) -> (i32, String) {
    let executable = resolve_qemu_img(cfg).and_then(|path| Ok(std::path::absolute(path)?));
    let qemu_img = match executable {
        Ok(path) => path,
        Err(error) => return (127, error.to_string()),
    };
    let (Some(parent), Some(file)) = (disk.parent(), disk.file_name()) else {
        return (127, "invalid disk path".into());
    };
    let mut argv = vec![qemu_img.display().to_string()];
    argv.extend(args.iter().map(|arg| arg.to_string()));
    argv.push(file.to_string_lossy().into_owned());
    let cwd = (!parent.as_os_str().is_empty()).then_some(parent);
    let result = capture_env(&argv, std::time::Duration::from_secs(120), None, cwd, None);
    (result.code, result.output)
}

pub fn disk_info(cfg: &Config, disk: &Path) -> Option<Value> {
    if !disk.exists() {
        return None;
    }
    let (rc, out) = qemu_img_capture(cfg, disk, &["info", "-U", "--output=json"]);
    if rc != 0 {
        return None;
    }
    serde_json::from_str(&out).ok()
}

pub fn disk_virtual_size(cfg: &Config, disk: &Path) -> Option<u64> {
    disk_info(cfg, disk)?
        .get("virtual-size")
        .and_then(Value::as_u64)
}

fn legacy_snapshot_names(cfg: &Config, disk: &Path) -> Vec<String> {
    if !disk.exists() {
        return Vec::new();
    }
    let (rc, out) = qemu_img_capture(cfg, disk, &["snapshot", "-l", "-U"]);
    if rc != 0 {
        return Vec::new();
    }
    snapshot_names_from_output(&out)
}

fn snapshot_names_from_output(out: &str) -> Vec<String> {
    let Ok(re) = Regex::new(r"^\s*(\d+)\s+(\S+)\s") else {
        return out
            .lines()
            .filter_map(snapshot_name_fallback)
            .map(str::to_string)
            .collect();
    };
    let mut names = Vec::new();
    for line in out.lines() {
        if let Some(captures) = re.captures(line) {
            names.push(captures[2].to_string());
        }
    }
    names
}

fn snapshot_name_fallback(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let end_id = line.find(|character: char| !character.is_ascii_digit())?;
    if end_id == 0 || !line[end_id..].starts_with(char::is_whitespace) {
        return None;
    }
    let name = line[end_id..].trim_start();
    let end_name = name.find(char::is_whitespace)?;
    (end_name > 0).then_some(&name[..end_name])
}

/// The machine no longer uses qcow2 snapshots. Disks made by older versions
/// still carry internal ones (blank, installed, desktop, browser, sealed,
/// chrome-*, reconfig-backup); delete them all so their clusters are freed.
/// The VM must be stopped (qemu-img needs the disk unlocked).
pub fn purge_legacy_snapshots(cfg: &Config) -> Result<()> {
    let disk = cfg.disk_path();
    let names = legacy_snapshot_names(cfg, &disk);
    if names.is_empty() {
        return Ok(());
    }
    let ctx = StepContext::new(cfg, "disk", "drop-snapshots")?;
    for name in names {
        ctx.log(&format!("deleting legacy qcow2 snapshot {name}"));
        ctx.run(
            &[
                resolve_qemu_img(cfg)?.display().to_string(),
                "snapshot".to_string(),
                "-d".to_string(),
                name,
                disk.display().to_string(),
            ],
            std::time::Duration::from_secs(300),
            true,
        )?;
    }
    Ok(())
}

pub fn delete_artifacts(ctx: &StepContext, state: &Value, force: bool) -> Result<()> {
    let mut errors = Vec::new();
    let artifacts = state
        .get("artifacts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for artifact in artifacts.iter().rev() {
        let path_text = artifact.get("path").and_then(Value::as_str).unwrap_or("");
        if path_text.is_empty() {
            continue;
        }
        let path = PathBuf::from(path_text);
        let symlink = std::fs::symlink_metadata(&path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false);
        if !path.exists() && !symlink {
            continue;
        }
        let kind = artifact
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("file");
        let expected = artifact.get("sha256").and_then(Value::as_str);
        if matches!(kind, "volume" | "dir") || expected.is_none() {
            ctx.log(&format!("remove {}", path.display()));
            if let Err(error) = remove_path(&path) {
                errors.push(format!("{}: {error}", path.display()));
            }
            continue;
        }
        let current = sha256_file(&path).unwrap_or_default();
        if Some(current.as_str()) != expected && !force {
            errors.push(format!(
                "refusing to delete modified file {} (sha256 changed; use --force)",
                path.display()
            ));
            continue;
        }
        ctx.log(&format!("remove {}", path.display()));
        if let Err(error) = std::fs::remove_file(&path) {
            errors.push(format!("{}: {error}", path.display()));
        }
    }
    if !errors.is_empty() {
        return message(format!("rollback incomplete:\n  {}", errors.join("\n  ")));
    }
    Ok(())
}

pub trait Step {
    fn id(&self) -> &str;
    fn title(&self) -> &str;
    fn scope(&self) -> &str {
        "instance"
    }

    fn check(&self, ctx: &mut StepContext) -> Result<CheckResult>;
    fn apply(&self, ctx: &mut StepContext) -> Result<()>;
    fn rollback(&self, ctx: &mut StepContext, state: &Value, force: bool) -> Result<()> {
        delete_artifacts(ctx, state, force)
    }

    fn scope_for(&self, cfg: &Config) -> String {
        if self.scope() == "global" {
            "global".to_string()
        } else {
            cfg.name.clone()
        }
    }

    fn state(&self, cfg: &Config) -> Option<Value> {
        load_state(&cfg.home, &self.scope_for(cfg), self.id())
    }

    fn save_applied(
        &self,
        cfg: &Config,
        ctx: &StepContext,
        status: &str,
        error: Option<&str>,
    ) -> Result<Value> {
        let artifacts: Vec<Value> = ctx.artifacts.iter().map(Artifact::as_value).collect();
        let mut state = Map::new();
        state.insert("step".to_string(), json!(self.id()));
        state.insert("title".to_string(), json!(self.title()));
        state.insert("scope".to_string(), json!(self.scope_for(cfg)));
        state.insert(
            "instance".to_string(),
            if self.scope() == "global" {
                Value::Null
            } else {
                json!(cfg.name)
            },
        );
        state.insert("status".to_string(), json!(status));
        state.insert("started_at".to_string(), json!(ctx.started_at));
        state.insert("finished_at".to_string(), json!(now_millis()));
        state.insert(
            "duration_s".to_string(),
            json!((ctx.elapsed() * 100.0).round() / 100.0),
        );
        state.insert("config_fingerprint".to_string(), json!(cfg.fingerprint()));
        state.insert(
            "disk_fingerprint".to_string(),
            json!(cfg.disk_fingerprint()),
        );
        state.insert(
            "seed_fingerprint".to_string(),
            json!(cfg.seed_fingerprint()),
        );
        state.insert("artifacts".to_string(), Value::Array(artifacts));
        state.insert("meta".to_string(), Value::Object(ctx.meta.clone()));
        state.insert("log".to_string(), json!(ctx.log_path.display().to_string()));
        if let Some(error) = error {
            state.insert("error".to_string(), json!(error));
            state.insert("failed_at".to_string(), json!(now_millis()));
        }
        let state = Value::Object(state);
        save_state(&cfg.home, &self.scope_for(cfg), self.id(), &state)?;
        Ok(state)
    }

    fn mark_rolled_back(
        &self,
        cfg: &Config,
        ctx: &StepContext,
        state: &Value,
        success: bool,
        error: Option<&str>,
    ) -> Result<Value> {
        let mut updated = state.as_object().cloned().unwrap_or_default();
        updated.insert(
            "status".to_string(),
            json!(if success {
                "rolled_back"
            } else {
                "rollback_failed"
            }),
        );
        updated.insert("rolled_back_at".to_string(), json!(now_millis()));
        updated.insert(
            "rollback_log".to_string(),
            json!(ctx.log_path.display().to_string()),
        );
        if let Some(error) = error {
            updated.insert("rollback_error".to_string(), json!(error));
        }
        let updated = Value::Object(updated);
        save_state(&cfg.home, &self.scope_for(cfg), self.id(), &updated)?;
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::{snapshot_name_fallback, snapshot_names_from_output};

    #[test]
    fn snapshot_name_fallback_matches_standard_qemu_rows() {
        let output = "ID        TAG                 VM SIZE                DATE\n  1       blank                0 B 2026-01-01\n  12      项目                 0 B 2026-01-02\ninvalid row\n";
        let expected = vec!["blank".to_string(), "项目".to_string()];
        assert_eq!(snapshot_names_from_output(output), expected);
        let fallback: Vec<String> = output
            .lines()
            .filter_map(snapshot_name_fallback)
            .map(str::to_string)
            .collect();
        assert_eq!(fallback, expected);
        assert_eq!(snapshot_name_fallback("1 only"), None);
        assert_eq!(snapshot_name_fallback("1 only "), Some("only"));
    }
}
