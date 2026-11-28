//! Native model discovery. Each provider answers for itself; there is no
//! sidecar and no shared model catalog.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::acp;
use super::acp::rpc::{Handler, Peer, RpcError};
use super::stdio::StdioSpec;
use crate::error::{Error, Result};
use crate::model::{AgentKind, AgentModel};
use crate::system::{CommandSpec, HostSystem, System};

const PROBE_TIMEOUT: Duration = Duration::from_secs(40);
const ANTIGRAVITY_ACP_TIMEOUT: Duration = Duration::from_secs(20);
const ACP_AUTH_REQUIRED: i64 = -32000;
const ANTIGRAVITY_EFFORT_SUFFIXES: &[&str] = &["xhigh", "high", "medium", "low", "max"];

pub fn models(
    agent: AgentKind,
    binary: &Path,
    cwd: &Path,
    env: Vec<(String, String)>,
) -> Result<Vec<AgentModel>> {
    if !cwd.is_dir() {
        return Err(Error::invalid("The workspace folder does not exist"));
    }
    match agent {
        AgentKind::Claude => Ok(claude_models()),
        AgentKind::Antigravity => discover_antigravity_models(binary, cwd, &env),
        AgentKind::Codex => {
            let Some(stdout) = probe(binary, cwd, &env, &["debug", "models"]) else {
                return Ok(Vec::new());
            };
            parse_codex_models(&stdout).map_err(|message| {
                Error::Unavailable(format!("Invalid Codex model catalog: {message}"))
            })
        }
        AgentKind::Opencode => {
            let found = probe(binary, cwd, &env, &["models"])
                .map(|stdout| parse_opencode_models(&stdout))
                .filter(|models| !models.is_empty());
            Ok(found.unwrap_or_else(opencode_fallback))
        }
        AgentKind::Pi => {
            if let Some(stdout) = probe(binary, cwd, &env, &["models", "--json"]) {
                let found = parse_json_models(&stdout).unwrap_or_default();
                if !found.is_empty() {
                    return Ok(found);
                }
            }
            if let Some(stdout) = probe(binary, cwd, &env, &["--list-models"]) {
                let found = parse_list_models(&stdout);
                if !found.is_empty() {
                    return Ok(found);
                }
            }
            Ok(Vec::new())
        }
    }
}

struct ProbeHandler;

impl Handler for ProbeHandler {
    fn notification(&self, _: &str, _: &Value) {}

    fn request(&self, method: &str, _: &Value) -> std::result::Result<Value, RpcError> {
        Err(RpcError::method_not_found(method))
    }

    fn exited(&self, _: Option<i32>, _: &str, _: bool) {}
}

enum AntigravityAcpOutcome {
    Models(Vec<AgentModel>),
    AuthRequired,
    Failed,
}

/// Ask `agy acp` for the account's live model catalog through a disposable
/// session in a private temp folder, then delete that empty session from disk
/// (matching Orca and T3 Code). Falls back to `agy models` if ACP fails for a
/// non-auth reason.
fn discover_antigravity_models(
    binary: &Path,
    cwd: &Path,
    env: &[(String, String)],
) -> Result<Vec<AgentModel>> {
    match discover_antigravity_via_acp(binary, env) {
        AntigravityAcpOutcome::Models(models) if !models.is_empty() => return Ok(models),
        AntigravityAcpOutcome::AuthRequired => {
            return Err(Error::Unavailable(
                "Sign in to Antigravity in Agents → Account to load models.".into(),
            ));
        }
        AntigravityAcpOutcome::Models(_) | AntigravityAcpOutcome::Failed => {}
    }

    if let Some(stdout) = probe(binary, cwd, env, &["models"]) {
        let models = parse_antigravity_cli_models(&stdout);
        if !models.is_empty() {
            return Ok(models);
        }
    }

    Ok(vec![model(
        crate::agents::antigravity::DEFAULT_MODEL,
        "Agent default",
        None,
        true,
    )])
}

fn discover_antigravity_via_acp(
    binary: &Path,
    env: &[(String, String)],
) -> AntigravityAcpOutcome {
    let Ok(temp) = tempfile::Builder::new()
        .prefix("cabin-agy-models-")
        .tempdir()
    else {
        return AntigravityAcpOutcome::Failed;
    };
    let Ok(peer) = Peer::spawn(
        StdioSpec {
            argv: vec![binary.display().to_string(), "acp".into()],
            cwd: temp.path(),
            env,
        },
        Arc::new(ProbeHandler),
    ) else {
        return AntigravityAcpOutcome::Failed;
    };
    let init = peer.request(
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": {"readTextFile": false, "writeTextFile": false},
                "terminal": false,
                "elicitation": {"form": {}, "url": {}},
            },
            "clientInfo": {"name": "cabin", "version": env!("CARGO_PKG_VERSION")},
        }),
        Some(ANTIGRAVITY_ACP_TIMEOUT),
    );
    if init.is_err() {
        peer.stop();
        return AntigravityAcpOutcome::Failed;
    }
    let reply = peer.request_raw(
        "session/new",
        json!({"cwd": temp.path().display().to_string(), "mcpServers": []}),
        Some(ANTIGRAVITY_ACP_TIMEOUT),
    );
    let (session_id, outcome) = match reply {
        Ok(Ok(result)) => {
            let sid = result["sessionId"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_string);
            (sid, AntigravityAcpOutcome::Models(parse_antigravity_acp_models(&result)))
        }
        Ok(Err(rpc_err)) if rpc_err.code == ACP_AUTH_REQUIRED => {
            (None, AntigravityAcpOutcome::AuthRequired)
        }
        _ => (None, AntigravityAcpOutcome::Failed),
    };
    peer.stop();
    if let Some(sid) = session_id
        && let Some(home) = env_home(env)
    {
        discard_antigravity_listing_session(&home, &sid);
    }
    outcome
}

fn env_home(env: &[(String, String)]) -> Option<PathBuf> {
    env.iter()
        .rev()
        .find(|(key, _)| key == "HOME")
        .map(|(_, value)| PathBuf::from(value))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Remove the disposable session's files from `~/.gemini/antigravity-cli`
/// after the probe process exits so listing models never pollutes history.
fn discard_antigravity_listing_session(home: &Path, session_id: &str) {
    if !is_uuid(session_id) {
        return;
    }
    let app_data = home.join(".gemini").join("antigravity-cli");
    let conversations = app_data.join("conversations");
    let db = conversations.join(format!("{session_id}.db"));
    if !db.is_file() {
        return;
    }
    for suffix in [".db", ".db-wal", ".db-shm", ".db-journal", ".meta"] {
        let _ = std::fs::remove_file(conversations.join(format!("{session_id}{suffix}")));
    }
    let _ = std::fs::remove_dir_all(app_data.join("brain").join(session_id));
    let _ = std::fs::remove_file(
        app_data
            .join("annotations")
            .join(format!("{session_id}.pbtxt")),
    );
}

fn split_antigravity_effort(id: &str) -> Option<(&str, &str)> {
    for suffix in ANTIGRAVITY_EFFORT_SUFFIXES {
        if let Some(family) = id.strip_suffix(&format!("-{suffix}"))
            && !family.is_empty()
        {
            return Some((family, suffix));
        }
    }
    None
}

fn strip_trailing_effort_label(name: &str) -> String {
    let trimmed = name.trim();
    if let Some(open) = trimmed.rfind(" (")
        && trimmed.ends_with(')')
    {
        return trimmed[..open].trim().to_string();
    }
    trimmed.to_string()
}

/// Group `<family>-<level>` variants into one family entry when a family has
/// multiple effort levels and is not itself a standalone model (matching
/// `agy acp`'s `configOptions` model selector and Orca's `antigravityModelChoices`).
fn antigravity_family_models(
    entries: &[(String, String)],
    current_id: Option<&str>,
) -> Vec<AgentModel> {
    let ids: HashSet<&str> = entries.iter().map(|(id, _)| id.as_str()).collect();
    let mut level_counts: HashMap<&str, usize> = HashMap::new();
    for (id, _) in entries {
        if let Some((family, _)) = split_antigravity_effort(id)
            && !ids.contains(family)
        {
            *level_counts.entry(family).or_default() += 1;
        }
    }
    let current_family = current_id.and_then(|id| {
        split_antigravity_effort(id)
            .filter(|(family, _)| level_counts.get(family).copied().unwrap_or(0) > 1)
            .map(|(family, _)| family)
    });
    let default_target = current_family.or(current_id);

    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for (id, name) in entries {
        let (resolved_id, resolved_name) = match split_antigravity_effort(id) {
            Some((family, _)) if level_counts.get(family).copied().unwrap_or(0) > 1 => {
                (family.to_string(), strip_trailing_effort_label(name))
            }
            _ => (id.clone(), name.clone()),
        };
        if resolved_id.is_empty() || !seen.insert(resolved_id.clone()) {
            continue;
        }
        models.push(model(
            resolved_id.clone(),
            if resolved_name.is_empty() {
                resolved_id
            } else {
                resolved_name
            },
            None,
            false,
        ));
    }
    promote_default(&mut models, default_target);
    models
}

fn promote_default(models: &mut Vec<AgentModel>, default_id: Option<&str>) {
    if models.is_empty() {
        return;
    }
    if let Some(target) = default_id
        && let Some(pos) = models.iter().position(|entry| entry.id == target)
        && pos > 0
    {
        let entry = models.remove(pos);
        models.insert(0, entry);
    }
    if let Some(first) = models.first_mut() {
        first.is_default = true;
    }
}

fn parse_antigravity_acp_models(result: &Value) -> Vec<AgentModel> {
    if let Some(option) = acp::config_options(&result["configOptions"])
        .into_iter()
        .find(|option| option.category.as_deref() == Some("model") || option.id == "model")
        && !option.choices.is_empty()
    {
        let mut seen = HashSet::new();
        let mut models = Vec::new();
        for choice in option.choices {
            if choice.value.is_empty() || !seen.insert(choice.value.clone()) {
                continue;
            }
            models.push(AgentModel {
                id: choice.value.clone(),
                name: if choice.name.is_empty() {
                    choice.value
                } else {
                    choice.name
                },
                provider: choice.group,
                description: choice.description,
                is_default: false,
            });
        }
        let current = (!option.current.is_empty()).then_some(option.current.as_str());
        promote_default(&mut models, current);
        return models;
    }

    let Some(available) = result.pointer("/models/availableModels").and_then(Value::as_array) else {
        return Vec::new();
    };
    let entries: Vec<(String, String)> = available
        .iter()
        .filter_map(|entry| {
            let id = entry["modelId"]
                .as_str()
                .or_else(|| entry["id"].as_str())?
                .trim();
            if id.is_empty() {
                return None;
            }
            let name = entry["name"].as_str().map(str::trim).unwrap_or(id);
            Some((id.to_string(), name.to_string()))
        })
        .collect();
    let current = result
        .pointer("/models/currentModelId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    antigravity_family_models(&entries, current)
}

fn parse_antigravity_cli_models(stdout: &str) -> Vec<AgentModel> {
    let clean = strip_ansi(stdout);
    let mut entries = Vec::new();
    for raw in clean.lines() {
        // `agy models` writes a `\r`-delimited spinner before the first row.
        let line = raw.rsplit('\r').next().unwrap_or(raw).trim();
        if line.is_empty() {
            continue;
        }
        let pair = line
            .split_once('\t')
            .map(|(id, name)| (id.trim(), name.trim()))
            .or_else(|| {
                line.find("  ")
                    .map(|gap| (line[..gap].trim(), line[gap..].trim()))
            });
        let Some((id, name)) = pair else {
            continue;
        };
        if id.is_empty()
            || name.is_empty()
            || id.contains(char::is_whitespace)
            || !id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':'))
        {
            continue;
        }
        entries.push((id.to_string(), name.to_string()));
    }
    antigravity_family_models(&entries, None)
}

fn probe(binary: &Path, cwd: &Path, env: &[(String, String)], args: &[&str]) -> Option<String> {
    let path = env
        .iter()
        .rev()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());
    let argv = std::iter::once(binary.display().to_string())
        .chain(args.iter().map(|arg| (*arg).to_string()));
    let mut spec = CommandSpec::new(argv)
        .env(env.iter().cloned())
        .timeout(PROBE_TIMEOUT);
    spec.cwd = Some(cwd.display().to_string());
    let output = HostSystem { path }.run(&spec).ok()?;
    output.success().then(|| output.combined())
}

fn model(
    id: impl Into<String>,
    name: impl Into<String>,
    provider: Option<String>,
    is_default: bool,
) -> AgentModel {
    AgentModel {
        id: id.into(),
        name: name.into(),
        provider,
        description: None,
        is_default,
    }
}

fn claude_models() -> Vec<AgentModel> {
    [
        ("default", "Default"),
        ("opus", "Opus"),
        ("fable", "Fable"),
        ("sonnet", "Sonnet"),
        ("haiku", "Haiku"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (id, name))| model(id, name, Some("anthropic".into()), index == 0))
    .collect()
}

fn parse_codex_models(stdout: &str) -> std::result::Result<Vec<AgentModel>, String> {
    let value: Value =
        serde_json::from_str(stdout).map_err(|error| format!("invalid JSON: {error}"))?;
    let rows = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| "no models array".to_string())?;
    let mut models = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        if row.get("visibility").and_then(Value::as_str) == Some("hide") {
            continue;
        }
        let Some(id) = row.get("slug").and_then(Value::as_str) else {
            continue;
        };
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        let name = row
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or(id);
        models.push(model(id, name, Some("openai".into()), models.is_empty()));
    }
    Ok(models)
}

fn parse_opencode_models(stdout: &str) -> Vec<AgentModel> {
    let clean = strip_ansi(stdout);
    let mut models = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in clean.lines() {
        let Some(full_id) = line.split_whitespace().find(|token| token.contains('/')) else {
            continue;
        };
        let (provider, id) = full_id.split_once('/').unwrap_or(("opencode", full_id));
        if !seen.insert(full_id.to_string()) {
            continue;
        }
        // The provider is the group; the name is the model's own id.
        models.push(model(full_id, id, Some(provider.to_string()), false));
    }
    if models.is_empty() {
        return models;
    }
    let default = models
        .iter()
        .position(|entry| entry.id == "openai/gpt-5.3-codex")
        .or_else(|| {
            models
                .iter()
                .position(|entry| entry.id.starts_with("openai/"))
        })
        .unwrap_or(0);
    if default > 0 {
        let entry = models.remove(default);
        models.insert(0, entry);
    }
    if let Some(first) = models.first_mut() {
        first.is_default = true;
    }
    models
}

fn opencode_fallback() -> Vec<AgentModel> {
    [
        "opencode/big-pickle",
        "opencode/deepseek-v4-flash-free",
        "anthropic/claude-sonnet-5",
        "anthropic/claude-opus-5",
        "openai/gpt-5.3-codex",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, id)| {
        let (provider, name) = id.split_once('/').unwrap_or(("", id));
        model(id, name, Some(provider.to_string()), index == 0)
    })
    .collect()
}

fn parse_json_models(stdout: &str) -> std::result::Result<Vec<AgentModel>, String> {
    let value: Value =
        serde_json::from_str(stdout).map_err(|error| format!("invalid JSON: {error}"))?;
    let rows = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| "no models array".to_string())?;
    let mut models = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        let provider = row
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let id = row
            .get("selector")
            .and_then(Value::as_str)
            .filter(|selector| !selector.is_empty())
            .map(str::to_string)
            .or_else(|| {
                let id = row.get("id").and_then(Value::as_str)?;
                Some(if provider.is_empty() {
                    id.to_string()
                } else {
                    format!("{provider}/{id}")
                })
            });
        let Some(id) = id else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let name = row
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_string();
        models.push(model(
            id,
            name,
            (!provider.is_empty()).then(|| provider.to_string()),
            models.is_empty(),
        ));
    }
    Ok(models)
}

fn parse_list_models(stdout: &str) -> Vec<AgentModel> {
    let mut models = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in stdout.lines() {
        let line = strip_ansi(raw);
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let (provider, name) = (parts[0], parts[1]);
        if provider == "provider" && name == "model" {
            continue;
        }
        if provider
            .chars()
            .any(|ch| !ch.is_alphanumeric() && ch != '-' && ch != '_')
        {
            continue;
        }
        let id = format!("{provider}/{name}");
        if !seen.insert(id.clone()) {
            continue;
        }
        models.push(model(
            id,
            name,
            Some(provider.to_string()),
            models.is_empty(),
        ));
    }
    models
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                let _ = chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_catalog_keeps_only_visible_entries() {
        let stdout = r#"{"models":[
          {"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol","visibility":"list"},
          {"slug":"hidden","display_name":"Hidden","visibility":"hide"},
          {"slug":"gpt-5.5","display_name":"GPT-5.5","visibility":"list"},
          {"slug":"gpt-5.5","display_name":"GPT-5.5","visibility":"list"}
        ]}"#;
        let models = parse_codex_models(stdout).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-5.6-sol");
        assert!(models[0].is_default);
    }

    #[test]
    fn opencode_output_is_cleaned_and_promoted() {
        let stdout = "\
\u{1b}[32mLoading models...\u{1b}[0m
opencode/big-pickle
anthropic/claude-sonnet-5
openai/gpt-5.3-codex
";
        let models = parse_opencode_models(stdout);
        assert_eq!(models[0].id, "openai/gpt-5.3-codex");
        assert_eq!(models[1].id, "opencode/big-pickle");
    }

    #[test]
    fn pi_json_catalog_uses_selector_and_deduplicates() {
        let stdout = r#"{"models":[
          {"provider":"relay","id":"a","selector":"relay/a","name":"A"},
          {"provider":"relay","id":"a","name":"duplicate"},
          {"provider":"local","id":"b","name":"B"}
        ]}"#;
        let models = parse_json_models(stdout).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "relay/a");
        assert_eq!(models[1].id, "local/b");
    }

    #[test]
    fn antigravity_acp_models_prefer_config_options_and_fall_back_to_legacy_families() {
        let with_config = json!({
            "sessionId": "15e18c99-ce2c-45c0-a4da-64aebc0a1cde",
            "configOptions": [{
                "id": "model",
                "name": "Model",
                "category": "model",
                "type": "select",
                "currentValue": "gemini-4-carbon",
                "options": [
                    { "value": "gemini-4-neon", "name": "Gemini 4 Neon" },
                    { "value": "gemini-4-carbon", "name": "Gemini 4 Carbon" },
                    { "value": "claude-opus-5-5", "name": "Claude Opus 5.5" }
                ]
            }]
        });
        let models = parse_antigravity_acp_models(&with_config);
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "gemini-4-carbon");
        assert_eq!(models[0].name, "Gemini 4 Carbon");
        assert!(models[0].is_default);
        assert_eq!(models[1].id, "gemini-4-neon");
        assert!(!models[1].is_default);

        let legacy_only = json!({
            "models": {
                "currentModelId": "claude-opus-5-5-high",
                "availableModels": [
                    { "modelId": "gemini-4-neon-high", "name": "Gemini 4 Neon (High)" },
                    { "modelId": "gemini-4-neon-low", "name": "Gemini 4 Neon (Low)" },
                    { "modelId": "claude-opus-5-5-medium", "name": "Claude Opus 5.5 (Medium)" },
                    { "modelId": "claude-opus-5-5-high", "name": "Claude Opus 5.5 (High)" },
                    { "modelId": "gpt-oss-120b-medium", "name": "GPT-OSS 120B (Medium)" }
                ]
            }
        });
        let fallback = parse_antigravity_acp_models(&legacy_only);
        assert_eq!(
            fallback.iter().map(|m| (m.id.as_str(), m.name.as_str(), m.is_default)).collect::<Vec<_>>(),
            vec![
                ("claude-opus-5-5", "Claude Opus 5.5", true),
                ("gemini-4-neon", "Gemini 4 Neon", false),
                ("gpt-oss-120b-medium", "GPT-OSS 120B (Medium)", false),
            ]
        );
    }

    #[test]
    fn antigravity_cli_models_strip_spinner_and_group_efforts() {
        let stdout = "\r⠋ Fetching available models...\rgemini-4-neon-high             Gemini 4 Neon (High)\n\
gemini-4-neon-low              Gemini 4 Neon (Low)\n\
gemini-pro-orchestrator        Orchestrator (Carbon)\n\
gpt-oss-120b-medium\tGPT-OSS 120B (Medium)\n";
        let models = parse_antigravity_cli_models(stdout);
        assert_eq!(
            models.iter().map(|m| (m.id.as_str(), m.name.as_str())).collect::<Vec<_>>(),
            vec![
                ("gemini-4-neon", "Gemini 4 Neon"),
                ("gemini-pro-orchestrator", "Orchestrator (Carbon)"),
                ("gpt-oss-120b-medium", "GPT-OSS 120B (Medium)"),
            ]
        );
        assert!(models[0].is_default);
    }

    #[test]
    fn antigravity_listing_session_discard_removes_only_matching_uuid_files() {
        let dir = tempfile::tempdir().unwrap();
        let sid = "15e18c99-ce2c-45c0-a4da-64aebc0a1cde";
        let app = dir.path().join(".gemini/antigravity-cli");
        std::fs::create_dir_all(app.join("conversations")).unwrap();
        std::fs::create_dir_all(app.join("brain").join(sid)).unwrap();
        std::fs::create_dir_all(app.join("annotations")).unwrap();
        std::fs::write(app.join("conversations").join(format!("{sid}.db")), b"").unwrap();
        std::fs::write(app.join("conversations").join(format!("{sid}.db-wal")), b"").unwrap();
        std::fs::write(app.join("annotations").join(format!("{sid}.pbtxt")), b"").unwrap();

        discard_antigravity_listing_session(dir.path(), "../escape");
        assert!(app.join("conversations").join(format!("{sid}.db")).exists());

        discard_antigravity_listing_session(dir.path(), sid);
        assert!(!app.join("conversations").join(format!("{sid}.db")).exists());
        assert!(!app.join("conversations").join(format!("{sid}.db-wal")).exists());
        assert!(!app.join("brain").join(sid).exists());
        assert!(!app.join("annotations").join(format!("{sid}.pbtxt")).exists());
    }
}
