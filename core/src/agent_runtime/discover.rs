//! Native model discovery. Each provider answers for itself; there is no
//! sidecar and no shared model catalog.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::model::{AgentKind, AgentModel};
use crate::system::{CommandSpec, HostSystem, System};

const PROBE_TIMEOUT: Duration = Duration::from_secs(40);

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
        // The account's model list arrives with the ACP session options.
        AgentKind::Antigravity => Ok(vec![model(
            crate::agents::antigravity::DEFAULT_MODEL,
            "Agent default",
            None,
            true,
        )]),
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
}
