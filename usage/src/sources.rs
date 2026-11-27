use std::cell::LazyCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Record, Result, SyncResult, insert, revision};

fn configured(name: &str, fallback: PathBuf) -> PathBuf {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or(fallback)
}

pub fn sync(conn: &mut Connection, home: &Path) -> Result<SyncResult> {
    let mut result = SyncResult::default();
    let codex = configured("CODEX_HOME", home.join(".codex"));
    let pi = match pi_sessions(home) {
        Ok(path) => Some(path),
        Err(error) => {
            result.errors.push(format!("Pi: {error}"));
            None
        }
    };
    let mut sources = vec![
        (
            "claude",
            configured("CLAUDE_CONFIG_DIR", home.join(".claude")).join("projects"),
        ),
        ("codex", codex.join("sessions")),
        ("codex", codex.join("archived_sessions")),
    ];
    if let Some(pi) = pi {
        sources.push(("pi", pi));
    }
    for (agent, root) in sources {
        let mut files = collect_files(root, |path, error| {
            result
                .errors
                .push(format!("{agent} {}: {error}", path.display()));
        });
        files.sort();
        for file in files {
            if let Err(error) = sync_file(conn, agent, &file, &mut result) {
                result.errors.push(format!("{}: {error}", file.display()));
            }
        }
    }
    let data = configured("XDG_DATA_HOME", home.join(".local/share"));
    let opencode = configured("OPENCODE_DB", data.join("opencode/opencode.db"));
    match opencode.try_exists() {
        Ok(true) => {
            if let Err(error) = sync_opencode(conn, &opencode, &mut result) {
                result.errors.push(format!("OpenCode: {error}"));
            }
        }
        Ok(false) => {}
        Err(error) => result.errors.push(format!("OpenCode: {error}")),
    }
    sync_antigravity(conn, home, &mut result);
    Ok(result)
}

fn pi_sessions(home: &Path) -> Result<PathBuf> {
    let agent = configured("PI_CODING_AGENT_DIR", home.join(".pi/agent"));
    let setting = match fs::read_to_string(agent.join("settings.json")) {
        Ok(data) => text(&serde_json::from_str::<Value>(&data)?, "sessionDir"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let configured = std::env::var("PI_CODING_AGENT_SESSION_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .or(setting);
    let Some(path) = configured else {
        return Ok(agent.join("sessions"));
    };
    let path = if path == "~" {
        home.to_path_buf()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(path)
    };
    if !path.is_absolute() {
        return Err("relative sessionDir requires a project working directory".into());
    }
    Ok(path)
}

fn collect_files(root: PathBuf, mut report_error: impl FnMut(&Path, io::Error)) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut directories = vec![root];
    while let Some(dir) = directories.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                report_error(&dir, error);
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    report_error(&dir, error);
                    continue;
                }
            };
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => directories.push(path),
                Ok(kind)
                    if kind.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") =>
                {
                    files.push(path);
                }
                Ok(_) => {}
                Err(error) => report_error(&path, error),
            }
        }
    }
    files
}

fn key(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn n(value: &Value, field: &str) -> u64 {
    value
        .get(field)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(i64::MAX as u64)
}
fn text(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(512).collect())
}
fn timestamp(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value.as_str().and_then(|v| {
            chrono::DateTime::parse_from_rfc3339(v)
                .ok()
                .map(|v| v.timestamp_millis())
        })
    })
}
fn stamp(value: &Value, fallback: i64) -> i64 {
    value
        .get("timestamp")
        .and_then(timestamp)
        .unwrap_or(fallback)
}
fn cost(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.0)
}
fn has_usage(r: &Record) -> bool {
    r.input > 0
        || r.output > 0
        || r.cache_read > 0
        || r.cache_write > 0
        || r.reported_cost.is_some()
}
fn empty_record(id: String, agent: &'static str, timestamp: i64) -> Record {
    Record {
        id,
        agent,
        session_id: None,
        model: "unknown".into(),
        provider: None,
        timestamp,
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        reported_cost: None,
        duration_ms: None,
        status: None,
        complete: true,
    }
}

pub(crate) fn sync_file(
    conn: &mut Connection,
    agent: &'static str,
    path: &Path,
    result: &mut SyncResult,
) -> Result<()> {
    let metadata = fs::metadata(path)?;
    let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    let rev = format!("{}:{modified}", metadata.len());
    let path_text = path.to_string_lossy().into_owned();
    if revision(conn, &path_text)?.as_deref() == Some(&rev) {
        return Ok(());
    }
    let path_key = LazyCell::new(|| key(&path_text));
    let fallback = (modified / 1_000_000).min(i64::MAX as u128) as i64;
    let mut reader = BufReader::new(File::open(path)?);
    let mut buf = Vec::new();
    let mut offset = 0u64;
    let mut incomplete = false;
    let mut session: Option<String> = None;
    let mut model = "unknown".to_string();
    let mut total: Option<(u64, u64, u64, u64)> = None;
    let mut previous = None;
    let mut codex_fork = false;
    let mut codex_cutoff = None;
    let mut codex_continuation = false;
    let tx = conn.transaction()?;
    let mut added = 0;
    loop {
        buf.clear();
        let count = reader.read_until(b'\n', &mut buf)?;
        if count == 0 {
            break;
        }
        let start = offset;
        offset += count as u64;
        // A final complete JSON object may omit its newline; retry the file on
        // the next sync because the writer may still append to that line.
        let has_newline = buf.ends_with(b"\n");
        if !has_newline {
            incomplete = true;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&buf) else {
            if !has_newline {
                break;
            }
            continue;
        };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        let mut record = match agent {
            "claude" if kind == "assistant" => {
                let Some(msg) = v.get("message") else {
                    continue;
                };
                let Some(usage) = msg.get("usage").filter(|v| v.is_object()) else {
                    continue;
                };
                let session_id = text(&v, "sessionId");
                let id = match text(msg, "id") {
                    Some(id) => format!(
                        "claude:{}",
                        key(&format!("{}:{id}", session_id.as_deref().unwrap_or("")))
                    ),
                    None => format!("claude:{}:{start}", *path_key),
                };
                let mut r = empty_record(id, agent, stamp(&v, fallback));
                r.session_id = session_id;
                r.model = text(msg, "model").unwrap_or_else(|| "unknown".into());
                r.input = n(usage, "input_tokens");
                r.output = n(usage, "output_tokens");
                r.cache_read = n(usage, "cache_read_input_tokens");
                r.cache_write = n(usage, "cache_creation_input_tokens");
                r.status = text(msg, "stop_reason");
                r.complete = r.status.is_some();
                r
            }
            "pi" => {
                if kind == "session" {
                    session = text(&v, "id");
                    continue;
                }
                let carrier = if kind == "message" {
                    v.get("message")
                } else if kind == "compaction" || kind == "branch_summary" {
                    Some(&v)
                } else {
                    None
                };
                let Some(msg) = carrier else {
                    continue;
                };
                if kind == "message"
                    && !matches!(
                        msg.get("role").and_then(Value::as_str),
                        Some("assistant" | "toolResult")
                    )
                {
                    continue;
                }
                let Some(usage) = msg.get("usage").filter(|v| v.is_object()) else {
                    continue;
                };
                let id = text(&v, "id").unwrap_or_else(|| start.to_string());
                let mut r =
                    empty_record(format!("pi:{}:{id}", *path_key), agent, stamp(&v, fallback));
                r.session_id = session.clone();
                r.model = text(msg, "responseModel")
                    .or_else(|| text(msg, "model"))
                    .unwrap_or_else(|| "unknown".into());
                r.provider = text(msg, "provider");
                r.input = n(usage, "input");
                r.output = n(usage, "output");
                r.cache_read = n(usage, "cacheRead");
                r.cache_write = n(usage, "cacheWrite");
                r.reported_cost = cost(usage.pointer("/cost/total"));
                r.status = text(msg, "stopReason");
                r.duration_ms = msg
                    .get("duration")
                    .and_then(Value::as_i64)
                    .filter(|v| *v > 0);
                r
            }
            "codex" => {
                let payload = &v["payload"];
                if kind == "session_meta" {
                    session = text(payload, "id").or_else(|| text(payload, "thread_id"));
                    let parent = text(payload, "forked_from_id").or_else(|| {
                        text(
                            &payload["source"]["subagent"]["thread_spawn"],
                            "parent_thread_id",
                        )
                    });
                    codex_continuation = payload["history_mode"] == "paginated"
                        || text(&payload["history_base"], "thread_id").as_ref() == session.as_ref()
                            && session.is_some();
                    codex_fork = parent.is_some() && !codex_continuation;
                    codex_cutoff = v.get("timestamp").and_then(timestamp);
                    continue;
                }
                if kind == "turn_context" {
                    if let Some(m) =
                        text(payload, "model").or_else(|| text(&payload["info"], "model"))
                    {
                        model = m;
                    }
                    continue;
                }
                if kind != "event_msg" {
                    continue;
                }
                if payload["type"] == "thread_settings_applied" {
                    if let Some(m) = text(&payload["thread_settings"], "model") {
                        model = m;
                    }
                    continue;
                }
                if payload["type"] != "token_count" || !payload["info"].is_object() {
                    continue;
                }
                let info = &payload["info"];
                if let Some(m) = text(info, "model")
                    .or_else(|| text(info, "model_name"))
                    .or_else(|| text(payload, "model"))
                {
                    model = m;
                }
                let current = info
                    .get("total_token_usage")
                    .filter(|v| v.is_object())
                    .map(counters);
                let last = info
                    .get("last_token_usage")
                    .filter(|v| v.is_object())
                    .map(counters);
                let signature = (current, last);
                if current.is_some() && previous == Some(signature) {
                    continue;
                }
                previous = Some(signature);
                let replayed_parent = if codex_fork {
                    let cutoff = codex_cutoff.ok_or(
                        "Codex fork has no timestamp; cannot safely distinguish inherited usage",
                    )?;
                    let time = v.get("timestamp").and_then(timestamp).ok_or("Codex fork usage has no timestamp; cannot safely distinguish inherited usage")?;
                    time <= cutoff
                } else {
                    false
                };
                let delta = if let Some(last) = last {
                    last
                } else if let Some(current) = current {
                    if codex_continuation && total.is_none() {
                        return Err(
                            "Codex continuation has no per-request usage or previous baseline"
                                .into(),
                        );
                    }
                    let base = total.unwrap_or_default();
                    (
                        current.0.saturating_sub(base.0),
                        current.1.saturating_sub(base.1),
                        current.2.saturating_sub(base.2),
                        current.3.saturating_sub(base.3),
                    )
                } else {
                    continue;
                };
                if let Some(current) = current {
                    let base = total.unwrap_or_default();
                    total = Some((
                        current.0.max(base.0),
                        current.1.max(base.1),
                        current.2.max(base.2),
                        current.3.max(base.3),
                    ));
                }
                if replayed_parent {
                    continue;
                }
                // Archiving moves the rollout but preserves its unique filename.
                let rollout = path.file_name().unwrap_or_default().to_string_lossy();
                let mut r = empty_record(
                    format!("codex:{}:{start}", key(&rollout)),
                    agent,
                    stamp(&v, fallback),
                );
                r.session_id = session.clone();
                r.model = model.clone();
                r.cache_read = delta.1.min(delta.0);
                r.cache_write = delta.2.min(delta.0 - r.cache_read);
                r.input = delta.0.saturating_sub(r.cache_read + r.cache_write);
                r.output = delta.3;
                r
            }
            _ => continue,
        };
        // Unpriced, zero-token placeholders are not requests.
        if !has_usage(&record) {
            continue;
        }
        if record.timestamp < 0 {
            record.timestamp = fallback;
        }
        if insert(&tx, &record)? {
            added += 1;
        }
    }
    if !incomplete {
        tx.execute("INSERT INTO source_revisions VALUES (?1,?2) ON CONFLICT(path) DO UPDATE SET revision=excluded.revision", params![path_text, rev])?;
    }
    tx.commit()?;
    result.imported += added;
    Ok(())
}

fn counters(v: &Value) -> (u64, u64, u64, u64) {
    (
        n(v, "input_tokens"),
        n(v, "cached_input_tokens"),
        n(v, "cache_write_input_tokens"),
        n(v, "output_tokens"),
    )
}

const ANTIGRAVITY: &str = "antigravity";
const AGY_WORKER: &str = "gemini-flash-worker-agy";
const HYDROGEN: &str = "gemini-4-hydrogen";
const MAX_ANTIGRAVITY_METADATA_ERRORS: usize = 3;

pub(crate) fn sync_antigravity(conn: &mut Connection, home: &Path, result: &mut SyncResult) {
    let Ok(home) = home.canonicalize() else {
        return;
    };
    let mut seen = HashSet::new();
    let mut metadata_errors = MAX_ANTIGRAVITY_METADATA_ERRORS;
    // cc-switch checks CLI first, then IDE, and shares one identity across both.
    for profile in ["antigravity-cli", "antigravity"] {
        let root = home.join(".gemini").join(profile);
        let brain = root.join("brain");
        if !brain.canonicalize().is_ok_and(|path| path == brain) {
            continue;
        }
        let Ok(entries) = fs::read_dir(brain) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(session) = entry.file_name().into_string() else {
                continue;
            };
            if session.is_empty()
                || !session
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                || !entry.file_type().is_ok_and(|kind| kind.is_dir())
            {
                continue;
            }
            let path = entry
                .path()
                .join(".system_generated/logs/transcript_full.jsonl");
            if !path.canonicalize().is_ok_and(|real| real == path)
                || !path.is_file()
                || !seen.insert(session.clone())
            {
                continue;
            }
            let db = root.join("conversations").join(format!("{session}.db"));
            let can_report = metadata_errors > 0;
            match sync_antigravity_transcript(conn, &session, &path, &db, can_report) {
                Ok((changed, warning)) => {
                    result.imported += changed;
                    if let Some(warning) = warning {
                        result
                            .errors
                            .push(format!("Antigravity {session} metadata: {warning}"));
                        metadata_errors -= 1;
                    }
                }
                Err(error) => result
                    .errors
                    .push(format!("Antigravity {session}: {error}")),
            }
        }
    }
}

fn file_revision(metadata: &fs::Metadata) -> Result<String> {
    let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{}:{modified}", metadata.len()))
}

fn antigravity_database_revision(db: &Path) -> Result<Option<String>> {
    match db.canonicalize() {
        Ok(path) if path == db => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let signature = |path: &Path| -> Result<Option<String>> {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => Ok(Some(file_revision(&meta)?)),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Some("-".into())),
            Err(error) => Err(error.into()),
        }
    };
    let Some(main) = signature(db)? else {
        return Ok(None);
    };
    let Some(wal) = signature(&db.with_extension("db-wal"))? else {
        return Ok(None);
    };
    // SQLite can also open these sidecars. Reject symlinks and special files.
    if signature(&db.with_extension("db-shm"))?.is_none()
        || signature(&db.with_extension("db-journal"))?.is_none()
    {
        return Ok(None);
    }
    Ok(Some(format!("{main}:{wal}")))
}

fn sync_antigravity_transcript(
    conn: &mut Connection,
    session: &str,
    path: &Path,
    db: &Path,
    can_report: bool,
) -> Result<(u64, Option<String>)> {
    let main = file_revision(&fs::metadata(path)?)?;
    let native_revision = antigravity_database_revision(db);
    let source = format!("{ANTIGRAVITY}:{session}");
    // Include the path so a conversation that moves between profiles is re-read.
    let rev = native_revision
        .as_ref()
        .ok()
        .map(|native| format!("v1:{main}:{}:{path:?}", native.as_deref().unwrap_or("-")));
    let tx = conn.transaction()?;
    if let Some(rev) = &rev
        && revision(&tx, &source)?.as_ref() == Some(rev)
    {
        return Ok((0, None));
    }
    let (steps, incomplete) = antigravity_steps(File::open(path)?, &source, session)?;
    let mut indices = steps
        .iter()
        .filter_map(|(&index, step)| matches!(step, AntigravityStep::Response(_)).then_some(index))
        .peekable();
    let generations = if indices.peek().is_none() {
        Ok(HashMap::new())
    } else {
        native_revision.and_then(|native| match native {
            Some(_) => Ok(antigravity_generations(db, indices)?),
            None => Ok(HashMap::new()),
        })
    };
    let mut selected = None;
    let mut changed = 0;
    for (index, step) in steps {
        match step {
            AntigravityStep::Selection(model) => selected = Some(model),
            AntigravityStep::Response(record) => {
                let generation = generations.as_ref().ok().and_then(|all| all.get(&index));
                if save_antigravity_step(&tx, record, selected.as_deref(), generation)? {
                    changed += 1;
                }
            }
        }
    }
    // Usage polls in the background. Persist one notification per unresolved
    // session so reopening the usage database doesn't bring the warning back.
    let warning_key = format!("{source}:metadata-error");
    let warning = match &generations {
        Ok(_) => {
            tx.execute("DELETE FROM source_revisions WHERE path=?1", [&warning_key])?;
            None
        }
        Err(error) if can_report && !sqlite_busy(error.as_ref()) => {
            let changed = tx.execute(
                "INSERT INTO source_revisions VALUES (?1,?2)
                 ON CONFLICT(path) DO NOTHING",
                params![warning_key, "reported"],
            )?;
            (changed != 0).then(|| error.to_string().chars().take(240).collect())
        }
        Err(_) => None,
    };
    if !incomplete
        && generations.is_ok()
        && let Some(rev) = rev
    {
        tx.execute(
            "INSERT INTO source_revisions VALUES (?1,?2) ON CONFLICT(path) DO UPDATE SET revision=excluded.revision",
            params![source, rev],
        )?;
    } else {
        tx.execute("DELETE FROM source_revisions WHERE path=?1", [&source])?;
    }
    tx.commit()?;
    Ok((changed, warning))
}

fn sqlite_busy(error: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        error
            .downcast_ref::<rusqlite::Error>()
            .and_then(rusqlite::Error::sqlite_error_code),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

// Adapted from the cc-switch v4.0.38 Antigravity transcript format and tests.
enum AntigravityStep {
    Selection(String),
    Response(Record),
}

fn antigravity_steps(
    file: File,
    source: &str,
    session: &str,
) -> Result<(BTreeMap<u64, AntigravityStep>, bool)> {
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut steps = BTreeMap::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok((steps, false));
        }
        let Ok(row) = serde_json::from_slice::<Value>(&line) else {
            if !line.ends_with(b"\n") && !line.iter().all(u8::is_ascii_whitespace) {
                return Ok((steps, true));
            }
            continue;
        };
        let Some(index) = row["step_index"].as_u64() else {
            continue;
        };
        let step = match row["type"].as_str() {
            Some("USER_INPUT") => row["content"]
                .as_str()
                .and_then(antigravity_selection)
                .map(AntigravityStep::Selection),
            Some("PLANNER_RESPONSE") if row["status"] == "DONE" => {
                let mut record = empty_record(
                    format!("{source}:{index}"),
                    ANTIGRAVITY,
                    antigravity_timestamp(&row["created_at"]),
                );
                let token = |field| row[field].as_i64().unwrap_or(0).max(0) as u64;
                record.session_id = Some(session.into());
                if let Some(model) = row["model"].as_str().and_then(antigravity_model_id) {
                    record.model = model;
                }
                // The agy mock verified these are fresh input and combined thinking/output.
                record.input = token("input_tokens");
                record.output = token("output_tokens");
                record.cache_read = token("cache_read_tokens");
                record.cache_write = token("cache_write_tokens");
                has_usage(&record).then_some(AntigravityStep::Response(record))
            }
            _ => None,
        };
        if let Some(step) = step {
            steps.insert(index, step);
        } else {
            // The latest valid line for this step supersedes the earlier snapshot.
            steps.remove(&index);
        }
    }
}

fn antigravity_timestamp(value: &Value) -> i64 {
    if let Some(time) = value.as_i64().or_else(|| value.as_f64().map(|n| n as i64)) {
        if time > 1_000_000_000_000 {
            time
        } else {
            time.max(0) * 1000
        }
    } else {
        timestamp(value).unwrap_or(0).max(0)
    }
}

fn antigravity_selection(content: &str) -> Option<String> {
    let start = content.find("Model Selection")?;
    let (_, model) = content[start..].split_once(" to ")?;
    let end = model
        .find(". ")
        .or_else(|| model.find('\n'))
        .or_else(|| model.find('`'));
    let model = model[..end.unwrap_or(model.len())]
        .trim()
        .trim_matches('.')
        .trim();
    if model.eq_ignore_ascii_case("none") {
        return None;
    }
    antigravity_model_id(model)
}

fn antigravity_model_id(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("unknown") {
        return None;
    }
    if !name.contains(' ') {
        return Some(name.chars().take(512).collect());
    }
    let joined = name.trim().to_ascii_lowercase().replace(['(', ')'], "");
    Some(
        joined
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-")
            .chars()
            .take(512)
            .collect(),
    )
}

fn orchestrator_mode(model: &str) -> Option<&'static str> {
    match model {
        "orchestrator-carbon" | "gemini-pro-orchestrator" => Some("gemini-4-carbon"),
        "orchestrator-argon"
        | "gemini-pro-orchestrator-argon"
        | "gemini-pro-orchestrator-g1-pro" => Some("gemini-4-argon"),
        "orchestrator-neon" | "gemini-pro-orchestrator-neon" => Some("gemini-4-neon"),
        _ => None,
    }
}

fn concrete_worker(model: &str) -> bool {
    !matches!(model, "unknown" | "gemini-flash" | AGY_WORKER) && orchestrator_mode(model).is_none()
}

fn save_antigravity_step(
    conn: &Connection,
    record: Record,
    selection: Option<&str>,
    generation: Option<&AntigravityGeneration>,
) -> Result<bool> {
    let native = generation.and_then(|g| g.model.as_deref());
    let explicit = (record.model != "unknown").then_some(record.model.as_str());
    let mut requested = explicit.or(selection).or(native).unwrap_or("unknown");
    let worker = requested == AGY_WORKER;
    let native_worker = native.filter(|model| concrete_worker(model));
    let mut model = if orchestrator_mode(requested).is_some() {
        native.unwrap_or(requested)
    } else if worker {
        native_worker.unwrap_or(HYDROGEN)
    } else {
        requested
    };
    let previous = if model == "unknown"
        || orchestrator_mode(model).is_some()
        || (worker && native_worker.is_none())
    {
        conn.prepare_cached(
            "SELECT u.model, s.model FROM usage_records u
            JOIN antigravity_selections s USING (id) WHERE u.id=?1 AND u.model!='unknown'",
        )?
        .query_row([&record.id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .optional()?
    } else {
        None
    };
    if let Some((old_model, old_requested)) = previous.as_ref() {
        if model == "unknown" {
            model = old_model;
            requested = old_requested;
        } else if worker {
            if (old_requested == AGY_WORKER && concrete_worker(old_model))
                || (orchestrator_mode(old_requested).is_some() && old_model.starts_with(HYDROGEN))
            {
                model = old_model;
            }
        } else if orchestrator_mode(old_requested) == orchestrator_mode(requested)
            && orchestrator_mode(old_model).is_none()
            && old_model != AGY_WORKER
        {
            model = old_model;
        }
    }
    let changed = conn.prepare_cached(
        "INSERT INTO usage_records (id,agent,session_id,model,timestamp,input,output,cache_read,cache_write,duration_ms,status,complete)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'DONE',1)
         ON CONFLICT(id) DO UPDATE SET model=excluded.model,input=excluded.input,output=excluded.output,
           cache_read=excluded.cache_read,cache_write=excluded.cache_write,
           duration_ms=COALESCE(excluded.duration_ms, usage_records.duration_ms)
         WHERE model!=excluded.model OR input!=excluded.input OR output!=excluded.output
           OR cache_read!=excluded.cache_read OR cache_write!=excluded.cache_write
           OR (excluded.duration_ms IS NOT NULL AND duration_ms IS NOT excluded.duration_ms)",
    )?.execute(params![record.id, record.agent, record.session_id, model, record.timestamp,
        record.input, record.output, record.cache_read, record.cache_write,
        generation.and_then(|g| g.duration_ms)])?;
    conn.prepare_cached(
        "INSERT INTO antigravity_selections VALUES (?1,?2)
         ON CONFLICT(id) DO UPDATE SET model=excluded.model WHERE model!=excluded.model",
    )?
    .execute(params![record.id, requested])?;
    Ok(changed != 0)
}

struct AntigravityGeneration {
    model: Option<String>,
    duration_ms: Option<i64>,
}

fn antigravity_generations(
    db: &Path,
    steps: impl Iterator<Item = u64>,
) -> rusqlite::Result<HashMap<u64, AntigravityGeneration>> {
    let mut steps = steps.peekable();
    let mut generations = HashMap::new();
    if steps.peek().is_none() {
        return Ok(generations);
    }
    let mut source = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    source.busy_timeout(std::time::Duration::ZERO)?;
    let tx = source.transaction()?;
    let has_tables: bool = tx.query_row(
        "SELECT count(*) = 2 FROM sqlite_schema WHERE type='table' AND name IN ('steps','gen_metadata')",
        [],
        |row| row.get(0),
    )?;
    if !has_tables {
        return Ok(generations);
    }
    let mut step_query = tx.prepare("SELECT metadata FROM steps WHERE idx=?1 AND step_type=15")?;
    let mut generation_query = tx.prepare("SELECT data FROM gen_metadata WHERE idx=?1")?;
    for step in steps {
        let Ok(index) = i64::try_from(step) else {
            continue;
        };
        let metadata = step_query
            .query_row([index], |row| Ok(row.get::<_, Vec<u8>>(0).ok()))
            .optional()?
            .flatten();
        let Some(reference) = metadata
            .as_deref()
            .and_then(|data| protobuf_bytes(data, 20))
        else {
            continue;
        };
        // Protobuf omits the generation index when it is zero.
        let Some(index) = protobuf_int(reference, 3) else {
            continue;
        };
        let Ok(index) = i64::try_from(index) else {
            continue;
        };
        let data = generation_query
            .query_row([index], |row| Ok(row.get::<_, Vec<u8>>(0).ok()))
            .optional()?
            .flatten();
        let Some(response) = data.as_deref().and_then(|data| protobuf_bytes(data, 1)) else {
            continue;
        };
        let name = |field| {
            std::str::from_utf8(protobuf_bytes(response, field)?)
                .ok()
                .and_then(antigravity_model_id)
        };
        let base = name(19);
        let mapped = match base.as_deref() {
            Some(AGY_WORKER) => Some(HYDROGEN),
            Some(model) => orchestrator_mode(model),
            None => None,
        };
        let model = name(21)
            .filter(|model| mapped.is_none() || concrete_worker(model))
            .or_else(|| mapped.map(str::to_owned))
            .or(base);
        let duration_ms = protobuf_duration(response, 11)
            .zip(protobuf_duration(response, 12))
            .and_then(|(wait, stream)| wait.checked_add(stream))
            .filter(|ms| *ms > 0);
        generations.insert(step, AntigravityGeneration { model, duration_ms });
    }
    Ok(generations)
}

fn protobuf_duration(data: &[u8], field: u64) -> Option<i64> {
    let duration = protobuf_bytes(data, field)?;
    let seconds = i64::try_from(protobuf_int(duration, 1)?).ok()?;
    let nanos = i64::try_from(protobuf_int(duration, 2)?).ok()?;
    if nanos >= 1_000_000_000 {
        return None;
    }
    seconds.checked_mul(1000)?.checked_add(nanos / 1_000_000)
}

fn protobuf_bytes(data: &[u8], field: u64) -> Option<&[u8]> {
    match protobuf_field(data, field, 2)? {
        Some(Protobuf::Bytes(bytes)) => Some(bytes),
        _ => None,
    }
}

fn protobuf_int(data: &[u8], field: u64) -> Option<u64> {
    match protobuf_field(data, field, 0)? {
        Some(Protobuf::Integer(value)) => Some(value),
        None => Some(0),
        _ => None,
    }
}

enum Protobuf<'a> {
    Integer(u64),
    Bytes(&'a [u8]),
    Fixed,
}

// The native schema uses ordinary length-delimited messages and varints. Skip
// unrelated fields and reject truncated or invalid wire data within a message.
fn protobuf_field(mut data: &[u8], field: u64, wire: u64) -> Option<Option<Protobuf<'_>>> {
    let mut found = None;
    while !data.is_empty() {
        let tag = protobuf_varint(&mut data)?;
        let number = tag >> 3;
        if number == 0 || number >= 1 << 29 {
            return None;
        }
        let kind = tag & 7;
        let value = match kind {
            0 => Protobuf::Integer(protobuf_varint(&mut data)?),
            2 => {
                let len = usize::try_from(protobuf_varint(&mut data)?).ok()?;
                let (bytes, rest) = data.split_at_checked(len)?;
                data = rest;
                Protobuf::Bytes(bytes)
            }
            1 | 5 => {
                data = data.get(if kind == 1 { 8 } else { 4 }..)?;
                Protobuf::Fixed
            }
            _ => return None,
        };
        if number == field {
            if kind != wire {
                return None;
            }
            found = Some(value);
        }
    }
    Some(found)
}

fn protobuf_varint(data: &mut &[u8]) -> Option<u64> {
    let mut value = 0;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = data.split_first()?;
        *data = rest;
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 127) << shift;
        if byte < 128 {
            return Some(value);
        }
    }
    None
}

pub(crate) fn sync_opencode(
    conn: &mut Connection,
    path: &Path,
    result: &mut SyncResult,
) -> Result<()> {
    let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    source.busy_timeout(std::time::Duration::from_secs(5))?;
    let tx = conn.transaction()?;
    let mut seen = HashSet::new();
    let mut added = 0;
    let mut queries = Vec::new();
    for (table, v2) in [("session_message", true), ("message", false)] {
        let exists: bool = source.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )?;
        if exists {
            queries.push(if v2 {
                "SELECT id,session_id,data,type,time_updated,0 AS priority FROM session_message WHERE type IN ('assistant','compaction')"
            } else {
                "SELECT id,session_id,data,NULL AS type,time_updated,1 AS priority FROM message"
            });
        }
    }
    if !queries.is_empty() {
        // In mixed stores the newest copy wins; V2 breaks ties.
        let sql = format!(
            "SELECT id,session_id,data,type FROM ({}) ORDER BY time_updated DESC,priority",
            queries.join(" UNION ALL ")
        );
        let mut stmt = source.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            if !seen.insert(id.clone()) {
                continue;
            }
            let session: String = row.get(1)?;
            let data: String = row.get(2)?;
            let message_type: Option<String> = row.get(3)?;
            let Ok(v) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            if message_type.is_none() && v["role"] != "assistant" {
                continue;
            }
            let complete = if message_type.as_deref() == Some("compaction") {
                matches!(v["status"].as_str(), Some("completed" | "failed"))
            } else {
                !v["time"]["completed"].is_null()
            };
            if !complete || !v["tokens"].is_object() {
                continue;
            }
            let time = &v["time"];
            let start = time["created"].as_i64().unwrap_or(0);
            let mut r = empty_record(format!("opencode:{}", key(&id)), "opencode", start);
            r.session_id = Some(session);
            r.model = text(&v, "modelID")
                .or_else(|| v["model"].as_str().map(str::to_string))
                .or_else(|| text(&v["model"], "id"))
                .unwrap_or_else(|| "unknown".into());
            r.provider = text(&v, "providerID").or_else(|| text(&v["model"], "providerID"));
            let tokens = &v["tokens"];
            r.input = n(tokens, "input");
            r.output = n(tokens, "output");
            r.cache_read = n(&tokens["cache"], "read");
            r.cache_write = n(&tokens["cache"], "write");
            r.reported_cost = cost(v.get("cost"));
            r.duration_ms = time["streamed"]
                .as_i64()
                .filter(|end| *end > start)
                .or_else(|| time["completed"].as_i64().filter(|end| *end > start))
                .filter(|_| start > 0)
                .map(|end| end - start);
            r.status = text(&v, "finish").or_else(|| text(&v, "status"));
            if has_usage(&r) && insert(&tx, &r)? {
                added += 1;
            }
        }
    }
    tx.commit()?;
    result.imported += added;
    Ok(())
}
