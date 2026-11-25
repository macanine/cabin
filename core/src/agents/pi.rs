use std::path::PathBuf;

use super::extensions::{McpCodec, json};
use super::{AgentAdapter, AgentSpec};
use crate::context::Context;
use crate::error::{Error, Result};
use crate::model::{AgentKind, McpServer, McpTransport, SkillEntry};

pub struct Pi;

static SPEC: AgentSpec = AgentSpec {
    kind: AgentKind::Pi,
    name: "Pi",
    binary: "pi",
    package: "@mariozechner/pi-coding-agent",
    config: ".pi/agent/settings.json",
    model_key: "defaultModel",
    auth_files: &[".pi/agent/auth.json"],
    auth_env: &["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GEMINI_API_KEY"],
};

impl AgentAdapter for Pi {
    fn skill_insert(&self, skill: &SkillEntry) -> Option<String> {
        Some(format!("/skill:{} ", skill.name))
    }

    fn spec(&self) -> &'static AgentSpec {
        &SPEC
    }

    fn mcp(&self) -> Option<&'static dyn McpCodec> {
        Some(&Mcp)
    }

    fn skills_dir(&self, ctx: &Context) -> PathBuf {
        agent_dir(ctx).join("skills")
    }

    fn project_skills_dir(&self) -> &'static str {
        ".pi/skills"
    }
}

fn agent_dir(ctx: &Context) -> PathBuf {
    #[cfg(not(test))]
    if let Some(path) = std::env::var_os("PI_CODING_AGENT_DIR") {
        return PathBuf::from(path);
    }
    ctx.paths.home().join(".pi/agent")
}

// Native MCP is read by Pi 1.0 and newer.
struct Mcp;
const MCP_KEY: &str = "mcpServers";

impl McpCodec for Mcp {
    fn path(&self, ctx: &Context) -> PathBuf {
        agent_dir(ctx).join("mcp.json")
    }

    fn can_disable(&self) -> bool {
        true
    }

    fn supports_sse(&self) -> bool {
        false
    }

    fn ids(&self, text: &str) -> Result<Vec<String>> {
        json::ids(text, MCP_KEY)
    }

    fn read(&self, text: &str) -> Result<Vec<McpServer>> {
        let mut servers = json::standard_servers(text, MCP_KEY, true)?;
        servers.retain(|server| !matches!(server.transport, McpTransport::Http { sse: true, .. }));
        Ok(servers)
    }

    fn upsert(
        &self,
        text: Option<&str>,
        server: &McpServer,
        previous: Option<&str>,
    ) -> Result<String> {
        if matches!(server.transport, McpTransport::Http { sse: true, .. }) {
            return Err(Error::invalid(
                "Pi requires streamable HTTP for remote MCP servers",
            ));
        }
        let normalized = server.name.replace('-', "_");
        for name in self.ids(text.unwrap_or_default())? {
            if name != server.name
                && Some(name.as_str()) != previous
                && name.replace('-', "_") == normalized
            {
                return Err(Error::Conflict(format!(
                    "Pi treats {} and {name} as the same MCP ID",
                    server.name
                )));
            }
        }
        json::update_standard(text, MCP_KEY, server, previous, true)
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
