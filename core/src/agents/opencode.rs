use super::extensions::{McpCodec, json};
use super::{AgentAdapter, AgentSpec};
use crate::context::Context;
use crate::error::{Error, Result};
use crate::model::{AgentKind, McpServer, McpTransport, SkillEntry};
use crate::system::CommandSpec;
use crate::util::clean_terminal_text;
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

pub struct OpenCode;

static SPEC: AgentSpec = AgentSpec {
    kind: AgentKind::Opencode,
    name: "OpenCode",
    binary: "opencode",
    package: "opencode-ai",
    config: ".config/opencode/opencode.json",
    // `provider/model`, for example `vendor/model-name`.
    model_key: "model",
    auth_files: &[".local/share/opencode/auth.json"],
    auth_env: &["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY"],
};

impl AgentAdapter for OpenCode {
    fn skill_insert(&self, skill: &SkillEntry) -> Option<String> {
        // OpenCode v2's skill tool takes the case-sensitive directory ID,
        // not the display name. ACP passes the mention to the model as text.
        let id = Path::new(&skill.path).file_name()?.to_str()?;
        if id.chars().any(|c| c.is_whitespace() || c.is_control() || c == '@') {
            return None;
        }
        Some(format!("@{id} "))
    }

    fn spec(&self) -> &'static AgentSpec {
        &SPEC
    }

    fn login_args(&self) -> &'static [&'static str] {
        &["auth", "login"]
    }

    /// The OpenCode TUI takes its first prompt as `--prompt`.
    fn prompt_args(&self, prompt: &str) -> Vec<String> {
        vec!["--prompt".into(), prompt.into()]
    }

    fn mcp(&self) -> Option<&'static dyn McpCodec> {
        Some(&Mcp)
    }

    fn skills_dir(&self, ctx: &crate::context::Context) -> PathBuf {
        ctx.paths.home().join(".config/opencode/skill")
    }

    fn project_skills_dir(&self) -> &'static str {
        ".opencode/skill"
    }

    fn skill_dirs(&self, ctx: &Context, workspace: Option<&Path>) -> Vec<(PathBuf, &'static str)> {
        // OpenCode resolves a project skill before a user skill with the same ID.
        let mut dirs = Vec::new();
        if let Some(root) = workspace {
            dirs.push((root.join(self.project_skills_dir()), "project"));
        }
        dirs.push((self.skills_dir(ctx), "user"));
        dirs
    }
}

/// `mcp` in opencode.json: `{type: local, command: [cmd, ...args],
/// environment, enabled}` or `{type: remote, url, headers, enabled}`.
struct Mcp;

const MCP_KEY: &str = "mcp";

impl McpCodec for Mcp {
    fn path(&self, ctx: &Context) -> PathBuf {
        ctx.paths.home().join(SPEC.config)
    }

    fn can_disable(&self) -> bool {
        true
    }

    fn ids(&self, text: &str) -> Result<Vec<String>> {
        json::ids(text, MCP_KEY)
    }

    fn read(&self, text: &str) -> Result<Vec<McpServer>> {
        let root = json::parse(Some(text))?;
        let Some(servers) = root.get(MCP_KEY).and_then(Value::as_object) else {
            return Ok(Vec::new());
        };
        Ok(servers
            .iter()
            .filter_map(|(name, entry)| {
                let remote = entry.get("type").and_then(Value::as_str) == Some("remote")
                    || entry.get("url").is_some();
                let transport = if remote {
                    McpTransport::Http {
                        url: entry.get("url")?.as_str()?.into(),
                        headers: json::strings(entry.get("headers"))?,
                        sse: false,
                    }
                } else {
                    let mut command = json::list(entry.get("command"))?.into_iter();
                    McpTransport::Stdio {
                        command: command.next()?,
                        args: command.collect(),
                        env: json::strings(entry.get("environment"))?,
                    }
                };
                let enabled = entry.get("enabled").and_then(Value::as_bool) != Some(false);
                Some(McpServer {
                    name: name.clone(),
                    transport,
                    enabled,
                })
            })
            .collect())
    }

    fn upsert(
        &self,
        text: Option<&str>,
        server: &McpServer,
        previous: Option<&str>,
    ) -> Result<String> {
        let mut root = json::parse(text)?;
        let servers = json::table(&mut root, MCP_KEY)?;
        let mut entry = json::take_entry(servers, &server.name, previous)?;
        match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                for key in ["url", "headers"] {
                    entry.remove(key);
                }
                entry.insert("type".into(), "local".into());
                let argv: Vec<Value> = std::iter::once(command.as_str())
                    .chain(args.iter().map(String::as_str))
                    .map(Value::from)
                    .collect();
                entry.insert("command".into(), Value::Array(argv));
                if env.is_empty() {
                    entry.remove("environment");
                } else {
                    entry.insert("environment".into(), json::object(env));
                }
            }
            McpTransport::Http { url, headers, .. } => {
                for key in ["command", "environment"] {
                    entry.remove(key);
                }
                entry.insert("type".into(), "remote".into());
                entry.insert("url".into(), url.as_str().into());
                if headers.is_empty() {
                    entry.remove("headers");
                } else {
                    entry.insert("headers".into(), json::object(headers));
                }
            }
        }
        entry.insert("enabled".into(), server.enabled.into());
        servers.insert(server.name.clone(), Value::Object(entry));
        json::finish(&root)
    }

    fn remove(&self, text: &str, name: &str) -> Result<String> {
        let mut root = json::parse(Some(text))?;
        if json::table(&mut root, MCP_KEY)?
            .shift_remove(name)
            .is_none()
        {
            return Err(Error::NotFound(format!("MCP server {name}")));
        }
        json::finish(&root)
    }
}

/// Ask OpenCode itself instead of treating a credential file as a login.
/// With no custom credential it can still use its official free providers,
/// so `authenticated` is false while the detail remains actionable.
pub(super) fn auth_status(ctx: &Context, binary: &Path) -> (bool, String) {
    let output = ctx.system.run(
        &CommandSpec::new([
            binary.display().to_string(),
            "auth".into(),
            "list".into(),
            "--pure".into(),
        ])
        .env([("PATH".into(), ctx.paths.child_path())])
        .timeout(Duration::from_secs(15)),
    );

    let providers = credential_providers(&ctx.paths.home().join(".local/share/opencode/auth.json"));
    let env_signed_in = SPEC
        .auth_env
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.is_empty()));

    match output {
        Ok(output) if output.success() => {
            let text = clean_terminal_text(&output.combined());
            let none = text
                .lines()
                .any(|line| line.trim().eq_ignore_ascii_case("0 credentials"));
            if !none {
                let names = if providers.is_empty() {
                    "custom provider".to_string()
                } else {
                    providers.join(", ")
                };
                return (true, format!("Credentialed OpenCode provider: {names}"));
            }
            if env_signed_in {
                let names: Vec<_> = SPEC
                    .auth_env
                    .iter()
                    .filter(|name| std::env::var(name).is_ok_and(|value| !value.is_empty()))
                    .copied()
                    .collect();
                return (
                    true,
                    format!("Provider key from the environment: {}", names.join(", ")),
                );
            }
            (
                false,
                "No custom credentials. Official OpenCode free providers are available.".into(),
            )
        }
        Ok(output) => {
            let detail = clean_terminal_text(&output.combined());
            let detail = detail.lines().next().unwrap_or("").trim();
            if env_signed_in {
                return (true, "Provider key from the environment".into());
            }
            (
                false,
                if detail.is_empty() {
                    "OpenCode is not signed in for a custom provider.".into()
                } else {
                    format!("OpenCode is not signed in for a custom provider: {detail}")
                },
            )
        }
        Err(error) => (
            env_signed_in,
            if env_signed_in {
                "Provider key from the environment".into()
            } else {
                format!("Could not check OpenCode login: {error}")
            },
        ),
    }
}

fn credential_providers(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut names: Vec<_> = value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}
