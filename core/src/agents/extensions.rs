//! MCP servers and skills of the agent CLIs, managed in their native files.
//!
//! The formats and folders are agent facts and live in `agents/*.rs` (an
//! `McpCodec` per agent, `skills_dir`/`project_skills_dir` on the adapter);
//! this module holds the mechanics shared by all of them. Every change runs
//! as an operation with a backup and a rollback.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{ConfigWrite, Transform, adapter, backup_dir};
use crate::context::Context;
use crate::error::{Error, Result};
use crate::fsx;
use crate::model::{AgentExtensions, AgentKind, McpServer, McpTransport, SkillEntry, SkillFile};
use crate::ops::{FnStep, Operation, StepCtx};

/// One agent's MCP config file and how to read and edit it.
pub trait McpCodec: Sync {
    fn path(&self, ctx: &Context) -> PathBuf;
    /// Whether the format can keep a server but switch it off.
    fn can_disable(&self) -> bool;
    fn supports_sse(&self) -> bool {
        true
    }
    fn read(&self, text: &str) -> Result<Vec<McpServer>>;
    /// Names include entries the normalized reader does not understand.
    fn ids(&self, text: &str) -> Result<Vec<String>>;
    fn shelve(&self, _text: &str, _name: &str) -> Result<Option<Value>> {
        Ok(None)
    }
    fn unshelve(&self, text: Option<&str>, _name: &str, _entry: &Value) -> Result<String> {
        Ok(text.unwrap_or_default().to_owned())
    }
    /// Add or replace `server`; `previous` is its old name when renamed.
    /// Fields the UI does not edit are kept.
    fn upsert(
        &self,
        text: Option<&str>,
        server: &McpServer,
        previous: Option<&str>,
    ) -> Result<String>;
    fn remove(&self, text: &str, name: &str) -> Result<String>;
}

/// The JSON helpers the JSON-based codecs share.
pub(super) mod json {
    use super::*;

    pub fn parse(text: Option<&str>) -> Result<Value> {
        match text {
            Some(text) if !text.trim().is_empty() => serde_json::from_str(text).map_err(|error| {
                Error::invalid(format!(
                    "The MCP config is not plain JSON ({error}); fix it by hand first"
                ))
            }),
            _ => Ok(Value::Object(Default::default())),
        }
    }

    pub fn table<'a>(
        root: &'a mut Value,
        key: &str,
    ) -> Result<&'a mut serde_json::Map<String, Value>> {
        let object = root
            .as_object_mut()
            .ok_or_else(|| Error::invalid("The MCP config is not a JSON object"))?;
        let entry = object
            .entry(key.to_string())
            .or_insert_with(|| Value::Object(Default::default()));
        entry
            .as_object_mut()
            .ok_or_else(|| Error::invalid(format!("`{key}` in the MCP config is not an object")))
    }

    pub fn ids(text: &str, key: &str) -> Result<Vec<String>> {
        let mut root = parse(Some(text))?;
        Ok(table(&mut root, key)?.keys().cloned().collect())
    }

    pub fn strings(value: Option<&Value>) -> Option<BTreeMap<String, String>> {
        match value {
            Some(value) => value
                .as_object()?
                .iter()
                .map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                .collect(),
            None => Some(BTreeMap::new()),
        }
    }

    pub fn list(value: Option<&Value>) -> Option<Vec<String>> {
        match value {
            Some(value) => value
                .as_array()?
                .iter()
                .map(|value| value.as_str().map(str::to_owned))
                .collect(),
            None => Some(Vec::new()),
        }
    }

    pub fn object(map: &BTreeMap<String, String>) -> Value {
        Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect(),
        )
    }

    /// Take the old entry (renamed or not) so unknown fields survive.
    pub fn take_entry(
        servers: &mut serde_json::Map<String, Value>,
        name: &str,
        previous: Option<&str>,
    ) -> Result<serde_json::Map<String, Value>> {
        let old = previous.unwrap_or(name);
        if old != name && servers.contains_key(name) {
            return Err(Error::Conflict(format!(
                "An MCP server named {name} already exists"
            )));
        }
        Ok(match servers.shift_remove(old) {
            Some(Value::Object(map)) => map,
            _ => Default::default(),
        })
    }

    pub fn standard_servers(text: &str, key: &str, can_disable: bool) -> Result<Vec<McpServer>> {
        let mut root = parse(Some(text))?;
        let servers = table(&mut root, key)?;
        Ok(servers
            .iter()
            .filter_map(|(name, entry)| {
                let transport_type = match entry.get("type") {
                    Some(value) => Some(value.as_str()?),
                    None => None,
                };
                if !matches!(
                    transport_type,
                    None | Some("stdio" | "http" | "sse" | "streamable-http")
                ) {
                    return None;
                }
                let transport = match entry.get("url") {
                    Some(url) => McpTransport::Http {
                        url: url.as_str()?.into(),
                        headers: strings(entry.get("headers"))?,
                        sse: entry.get("type").and_then(Value::as_str) == Some("sse"),
                    },
                    None => McpTransport::Stdio {
                        command: entry.get("command")?.as_str()?.into(),
                        args: list(entry.get("args"))?,
                        env: strings(entry.get("env"))?,
                    },
                };
                Some(McpServer {
                    name: name.clone(),
                    transport,
                    enabled: !can_disable
                        || entry.get("enabled").and_then(Value::as_bool) != Some(false),
                })
            })
            .collect())
    }

    pub fn update_standard(
        text: Option<&str>,
        key: &str,
        server: &McpServer,
        previous: Option<&str>,
        can_disable: bool,
    ) -> Result<String> {
        let mut root = parse(text)?;
        let servers = table(&mut root, key)?;
        let mut entry = take_entry(servers, &server.name, previous)?;
        match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                for key in ["url", "headers"] {
                    entry.remove(key);
                }
                entry.insert("type".into(), "stdio".into());
                entry.insert("command".into(), command.as_str().into());
                entry.insert(
                    "args".into(),
                    args.iter().map(|a| Value::from(a.as_str())).collect(),
                );
                if env.is_empty() {
                    entry.remove("env");
                } else {
                    entry.insert("env".into(), object(env));
                }
            }
            McpTransport::Http { url, headers, sse } => {
                for key in ["command", "args", "env"] {
                    entry.remove(key);
                }
                entry.insert("type".into(), if *sse { "sse" } else { "http" }.into());
                entry.insert("url".into(), url.as_str().into());
                if headers.is_empty() {
                    entry.remove("headers");
                } else {
                    entry.insert("headers".into(), object(headers));
                }
            }
        }
        if can_disable {
            entry.insert("enabled".into(), server.enabled.into());
        }
        servers.insert(server.name.clone(), Value::Object(entry));
        finish(&root)
    }

    pub fn finish(root: &Value) -> Result<String> {
        let mut text = serde_json::to_string_pretty(root)?;
        text.push('\n');
        Ok(text)
    }
}

/// MCP server names become config keys and TOML table names.
pub fn valid_mcp_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Error::invalid(
            "Use 1–64 letters, digits, '-' or '_' for the MCP server name",
        ));
    }
    Ok(())
}

pub(super) fn validate_server(server: &McpServer) -> Result<()> {
    valid_mcp_name(&server.name)?;
    match &server.transport {
        McpTransport::Stdio { command, env, .. } => {
            if command.trim().is_empty() {
                return Err(Error::invalid(
                    "Enter the command that starts the MCP server",
                ));
            }
            if env.keys().any(|key| key.is_empty() || key.contains('=')) {
                return Err(Error::invalid(
                    "Environment variable names cannot be empty or contain '='",
                ));
            }
        }
        McpTransport::Http { url, .. } => {
            if !url::Url::parse(url)
                .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
            {
                return Err(Error::invalid(
                    "The MCP server URL must start with http:// or https://",
                ));
            }
        }
    }
    Ok(())
}

/// Skill names are folder names and invocation words (Agent Skills rules).
pub fn valid_skill_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");
    if !ok {
        return Err(Error::invalid(
            "Use 1–64 lowercase letters, digits and single hyphens for the skill name",
        ));
    }
    Ok(())
}

pub(super) fn mcp_backup_dir(ctx: &Context, kind: AgentKind) -> PathBuf {
    // Kept apart from the main config's backups, which restore to that file.
    backup_dir(ctx, kind).join("mcp")
}

fn skills_backup_dir(ctx: &Context, kind: AgentKind) -> PathBuf {
    backup_dir(ctx, kind).join("skills")
}

pub fn user_skills_dir(ctx: &Context, kind: AgentKind) -> PathBuf {
    adapter(kind).skills_dir(ctx)
}

/// Read the `name` and `description` of a SKILL.md front matter.
pub fn skill_front_matter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let (mut name, mut description) = (None, None);
    let mut folded: Option<String> = None;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(text) = folded.as_mut() {
            if line.starts_with(' ') || line.starts_with('\t') {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(line.trim());
                continue;
            }
            description = folded.take().filter(|t| !t.is_empty());
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let unquoted = if let Some(inner) =
            value.strip_prefix('"').and_then(|v| v.strip_suffix('"'))
        {
            inner.replace("\\\"", "\"").replace("\\\\", "\\")
        } else if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
            inner.replace("''", "'")
        } else {
            value.to_string()
        };
        match key.trim() {
            "name" => name = Some(unquoted).filter(|v| !v.is_empty()),
            "description" if matches!(value, ">" | "|" | ">-" | "|-") => {
                folded = Some(String::new())
            }
            "description" => description = Some(unquoted).filter(|v| !v.is_empty()),
            _ => {}
        }
    }
    if let Some(text) = folded {
        description = Some(text).filter(|t| !t.is_empty());
    }
    (name, description)
}

pub(super) fn scan_skills(dir: &Path, scope: &str) -> Vec<SkillEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut skills: Vec<SkillEntry> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let text = std::fs::read_to_string(path.join("SKILL.md")).ok()?;
            let (name, description) = skill_front_matter(&text);
            let folder = path.file_name()?.to_string_lossy().into_owned();
            Some(SkillEntry {
                name: name.unwrap_or(folder),
                description: description.unwrap_or_default(),
                path: path.display().to_string(),
                scope: scope.into(),
            })
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

pub fn list(ctx: &Context, kind: AgentKind, workspace: Option<&Path>) -> AgentExtensions {
    let adapter = adapter(kind);
    let codec = adapter.mcp();
    let (mcp_servers, mcp_error) = match codec {
        Some(codec) => match std::fs::read_to_string(codec.path(ctx)) {
            Ok(text) => match codec.read(&text) {
                Ok(servers) => (servers, None),
                Err(error) => (Vec::new(), Some(error.to_string())),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        },
        None => (Vec::new(), None),
    };
    let user_dir = user_skills_dir(ctx, kind);
    let skills = adapter
        .skill_dirs(ctx, workspace)
        .into_iter()
        .flat_map(|(dir, scope)| scan_skills(&dir, scope))
        .collect();
    AgentExtensions {
        kind,
        mcp_path: codec.map(|codec| codec.path(ctx).display().to_string()),
        mcp_can_disable: codec.is_some_and(|codec| codec.can_disable()),
        mcp_servers,
        mcp_error,
        skills_path: user_dir.display().to_string(),
        skills,
    }
}

fn codec(kind: AgentKind) -> Result<&'static dyn McpCodec> {
    adapter(kind).mcp().ok_or_else(|| {
        Error::invalid(format!(
            "{} has no built-in MCP support",
            adapter(kind).spec().name
        ))
    })
}

fn mcp_write(ctx: &Context, kind: AgentKind, transform: Transform) -> Result<ConfigWrite> {
    Ok(ConfigWrite {
        path: codec(kind)?.path(ctx),
        kind,
        backup_dir: mcp_backup_dir(ctx, kind),
        transform,
        content: None,
        backup: None,
    })
}

fn key(kind: AgentKind) -> String {
    format!("agent-mcp-{}", kind.as_str())
}

pub fn save_mcp_operation(
    ctx: &Context,
    kind: AgentKind,
    server: McpServer,
    previous: Option<String>,
) -> Result<Operation> {
    validate_server(&server)?;
    let codec = codec(kind)?;
    if !server.enabled && !codec.can_disable() {
        return Err(Error::invalid(format!(
            "{} cannot keep a server switched off; remove it instead",
            adapter(kind).spec().name
        )));
    }
    let title = format!(
        "{}: save MCP server {}",
        adapter(kind).spec().name,
        server.name
    );
    let transform: Transform =
        Box::new(move |current| codec.upsert(current, &server, previous.as_deref()));
    Ok(Operation::new("agent.mcpSave", title)
        .key(key(kind))
        .step(mcp_write(ctx, kind, transform)?))
}

pub fn remove_mcp_operation(ctx: &Context, kind: AgentKind, name: String) -> Result<Operation> {
    let codec = codec(kind)?;
    let title = format!("{}: remove MCP server {name}", adapter(kind).spec().name);
    let transform: Transform = Box::new(move |current| {
        let current = current.ok_or_else(|| Error::NotFound(format!("MCP server {name}")))?;
        codec.remove(current, &name)
    });
    Ok(Operation::new("agent.mcpRemove", title)
        .key(key(kind))
        .step(mcp_write(ctx, kind, transform)?))
}

pub fn set_mcp_enabled_operation(
    ctx: &Context,
    kind: AgentKind,
    name: String,
    enabled: bool,
) -> Result<Operation> {
    let codec = codec(kind)?;
    if !codec.can_disable() {
        return Err(Error::invalid(format!(
            "{} cannot switch an MCP server off; remove it instead",
            adapter(kind).spec().name
        )));
    }
    let title = format!(
        "{}: {} MCP server {name}",
        adapter(kind).spec().name,
        if enabled { "enable" } else { "disable" }
    );
    let transform: Transform = Box::new(move |current| {
        let text = current.ok_or_else(|| Error::NotFound(format!("MCP server {name}")))?;
        let mut server = codec
            .read(text)?
            .into_iter()
            .find(|server| server.name == name)
            .ok_or_else(|| Error::NotFound(format!("MCP server {name}")))?;
        server.enabled = enabled;
        codec.upsert(Some(text), &server, None)
    });
    Ok(Operation::new("agent.mcpToggle", title)
        .key(key(kind))
        .step(mcp_write(ctx, kind, transform)?))
}

/// Copy one MCP server into other agents' configs. Agents that already have
/// a server with that name, or have no MCP support, are skipped.
pub fn copy_mcp_operation(
    ctx: &Context,
    from: AgentKind,
    name: String,
    targets: Vec<AgentKind>,
) -> Result<Operation> {
    let text = std::fs::read_to_string(codec(from)?.path(ctx))
        .map_err(|error| Error::io("read MCP config", error))?;
    let server = codec(from)?
        .read(&text)?
        .into_iter()
        .find(|server| server.name == name)
        .ok_or_else(|| Error::NotFound(format!("MCP server {name}")))?;
    let mut operation = Operation::new(
        "agent.mcpCopy",
        format!("Copy MCP server {name} from {}", adapter(from).spec().name),
    )
    .key("agent-mcp-copy");
    let mut any = false;
    for target in targets.into_iter().filter(|target| *target != from) {
        let Some(codec) = adapter(target).mcp() else {
            continue;
        };
        let existing = std::fs::read_to_string(codec.path(ctx)).ok();
        if let Some(text) = &existing
            && codec.read(text)?.iter().any(|s| s.name == name)
        {
            continue;
        }
        let mut copy = server.clone();
        copy.enabled = copy.enabled || !codec.can_disable();
        let transform: Transform = Box::new(move |current| codec.upsert(current, &copy, None));
        operation = operation.step(mcp_write(ctx, target, transform)?);
        any = true;
    }
    if !any {
        return Err(Error::Conflict(
            "Every other agent with MCP support already has this server".into(),
        ));
    }
    Ok(operation)
}

/// A skill path must sit directly in one of the agent's skill folders.
pub(super) fn owned_skill(
    ctx: &Context,
    kind: AgentKind,
    path: &str,
    workspace: Option<&Path>,
) -> Result<PathBuf> {
    let path = PathBuf::from(path);
    let allowed = adapter(kind)
        .skill_dirs(ctx, workspace)
        .into_iter()
        .any(|(dir, _)| path.parent() == Some(dir.as_path()));
    let name_ok = path
        .file_name()
        .is_some_and(|name| !name.is_empty() && name != "." && name != "..");
    if !name_ok || !allowed || !path.join("SKILL.md").is_file() {
        return Err(Error::NotFound("skill".into()));
    }
    Ok(path)
}

/// The Markdown after a leading `---` front-matter block.
pub(super) fn skill_body(text: &str) -> String {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return text.trim().to_string();
    }
    let mut body = String::new();
    let mut in_front = true;
    for line in lines {
        if in_front {
            if line.trim() == "---" {
                in_front = false;
            }
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    body.trim().to_string()
}

/// Read a skill's SKILL.md for the settings editor.
pub fn read_skill(
    ctx: &Context,
    kind: AgentKind,
    path: &str,
    workspace: Option<&Path>,
) -> Result<SkillFile> {
    let dir = owned_skill(ctx, kind, path, workspace)?;
    let text = std::fs::read_to_string(dir.join("SKILL.md"))?;
    let (name, description) = skill_front_matter(&text);
    let folder = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let project = workspace
        .map(|root| root.join(adapter(kind).project_skills_dir()))
        .filter(|project| dir.starts_with(project));
    Ok(SkillFile {
        path: dir.display().to_string(),
        name: name.filter(|name| !name.is_empty()).unwrap_or(folder),
        description: description.unwrap_or_default(),
        body: skill_body(&text),
        scope: if project.is_some() { "project" } else { "user" }.into(),
    })
}

/// Delete a skill by moving its folder into the agent's backups.
pub fn delete_skill_operation(
    ctx: &Context,
    kind: AgentKind,
    path: String,
    workspace: Option<&Path>,
) -> Result<Operation> {
    let source = owned_skill(ctx, kind, &path, workspace)?;
    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let target = skills_backup_dir(ctx, kind).join(format!("{}-{name}", crate::util::now_millis()));
    let (from, to) = (source.clone(), target.clone());
    Ok(Operation::new(
        "agent.skillDelete",
        format!("{}: delete skill {name}", adapter(kind).spec().name),
    )
    .key(format!("agent-skills-{}", kind.as_str()))
    .step(FnStep::new(
        format!("Move {} to backups", source.display()),
        move |step: &StepCtx<'_>| {
            move_dir(&from, &to)?;
            step.log(format!("Kept a copy at {}", to.display()));
            Ok(())
        },
        move |_: &StepCtx<'_>| {
            if target.exists() && !source.exists() {
                move_dir(&target, &source)?;
            }
            Ok(())
        },
    )))
}

/// Copy a skill folder into other agents' user skill folders. Agents that
/// already have a skill with that folder name are skipped.
pub fn copy_skill_operation(
    ctx: &Context,
    from: AgentKind,
    path: String,
    targets: Vec<AgentKind>,
    workspace: Option<&Path>,
) -> Result<Operation> {
    let source = owned_skill(ctx, from, &path, workspace)?;
    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut operation = Operation::new(
        "agent.skillCopy",
        format!("Copy skill {name} from {}", adapter(from).spec().name),
    )
    .key("agent-skills-copy");
    let mut any = false;
    for target in targets.into_iter().filter(|target| *target != from) {
        let dest = user_skills_dir(ctx, target).join(&name);
        if dest.exists() {
            continue;
        }
        let (src, apply_dest, rollback_dest) = (source.clone(), dest.clone(), dest.clone());
        operation = operation.step(FnStep::new(
            format!("Copy to {}", dest.display()),
            move |step: &StepCtx<'_>| {
                copy_dir(&src, &apply_dest)?;
                step.log(format!("Copied to {}", apply_dest.display()));
                Ok(())
            },
            move |_: &StepCtx<'_>| {
                let _ = std::fs::remove_dir_all(&rollback_dest);
                Ok(())
            },
        ));
        any = true;
    }
    if !any {
        return Err(Error::Conflict(
            "Every other agent already has this skill".into(),
        ));
    }
    Ok(operation)
}

fn move_dir(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fsx::ensure_dir(parent)?;
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        // Another filesystem: copy, then remove the original.
        Err(_) => {
            copy_dir(from, to)?;
            std::fs::remove_dir_all(from).map_err(|error| Error::path("remove", from, error))
        }
    }
}

/// Copy a folder tree. Symlinks are copied as links, never followed.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fsx::ensure_dir(to)?;
    for entry in std::fs::read_dir(from).map_err(|error| Error::path("read", from, error))? {
        let entry = entry.map_err(|error| Error::path("read", from, error))?;
        let kind = entry
            .file_type()
            .map_err(|error| Error::path("read", &entry.path(), error))?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_symlink() {
            let link = std::fs::read_link(entry.path())
                .map_err(|error| Error::path("read", &entry.path(), error))?;
            std::os::unix::fs::symlink(&link, &target)
                .map_err(|error| Error::path("link", &target, error))?;
        } else {
            std::fs::copy(entry.path(), &target)
                .map_err(|error| Error::path("copy", &entry.path(), error))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OperationState;
    use crate::ops::OperationManager;
    use crate::paths::AppPaths;
    use crate::store::Store;
    use crate::system::fake::FakeSystem;
    use std::sync::Arc;

    fn context(dir: &Path) -> Context {
        Context {
            paths: AppPaths::new(dir.join("data"), dir.join("home")),
            store: Arc::new(Store::in_memory().unwrap()),
            system: Arc::new(FakeSystem::default()),
        }
    }

    fn stdio(name: &str) -> McpServer {
        McpServer {
            name: name.into(),
            transport: McpTransport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
                env: BTreeMap::from([("GITHUB_TOKEN".into(), "t".into())]),
            },
            enabled: true,
        }
    }

    fn remote(name: &str) -> McpServer {
        McpServer {
            name: name.into(),
            transport: McpTransport::Http {
                url: "https://mcp.example.com/mcp".into(),
                headers: BTreeMap::from([("Authorization".into(), "Bearer x".into())]),
                sse: false,
            },
            enabled: true,
        }
    }

    /// Every codec round-trips both transports, keeps unrelated settings and
    /// fields it does not edit, renames, and removes.
    #[test]
    fn codecs_round_trip_and_keep_other_settings() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let cases: [(AgentKind, &str, &str); 4] = [
            (
                AgentKind::Claude,
                "{\"theme\": \"dark\", \"mcpServers\": {\"keep\": {\"command\": \"k\", \"timeout\": 5}}}",
                "theme",
            ),
            (
                AgentKind::Codex,
                "# my settings\nmodel = \"o3\"\n\n[mcp_servers.keep]\ncommand = \"k\"\nstartup_timeout_sec = 5\n",
                "my settings",
            ),
            (
                AgentKind::Opencode,
                "{\"theme\": \"dark\", \"mcp\": {\"keep\": {\"type\": \"local\", \"command\": [\"k\"], \"timeout\": 5}}}",
                "theme",
            ),
            (
                AgentKind::Pi,
                r#"{"settings":{}, "mcpServers":{"keep":{"command":"k","timeout":5}}}"#,
                "settings",
            ),
        ];
        for (kind, start, marker) in cases {
            let codec = adapter(kind).mcp().unwrap();
            let text = codec.upsert(Some(start), &stdio("github"), None)?;
            let text = codec.upsert(Some(&text), &remote("docs"), None)?;
            let servers = codec.read(&text)?;
            assert_eq!(servers.len(), 3, "{kind:?}: {text}");
            assert_eq!(
                servers.iter().find(|s| s.name == "github"),
                Some(&stdio("github")),
                "{kind:?}"
            );
            assert_eq!(
                servers.iter().find(|s| s.name == "docs"),
                Some(&remote("docs")),
                "{kind:?}"
            );
            assert!(
                text.contains(marker),
                "{kind:?} lost other settings: {text}"
            );
            // Editing `keep` keeps the field the UI does not know about.
            let mut keep = servers.iter().find(|s| s.name == "keep").unwrap().clone();
            keep.name = "kept".into();
            let text = codec.upsert(Some(&text), &keep, Some("keep"))?;
            assert!(
                text.contains('5'),
                "{kind:?} dropped an unknown field: {text}"
            );
            assert!(codec.read(&text)?.iter().all(|s| s.name != "keep"));
            assert!(
                codec
                    .upsert(Some(&text), &stdio("docs"), Some("kept"))
                    .is_err(),
                "rename onto an existing name"
            );
            if codec.can_disable() {
                let mut off = stdio("github");
                off.enabled = false;
                let text = codec.upsert(Some(&text), &off, None)?;
                assert!(
                    !codec
                        .read(&text)?
                        .iter()
                        .find(|s| s.name == "github")
                        .unwrap()
                        .enabled
                );
            }
            let text = codec.remove(&text, "github")?;
            assert!(codec.read(&text)?.iter().all(|s| s.name != "github"));
            assert!(codec.remove(&text, "missing").is_err());
            let _ = ctx.paths.home();
        }
        Ok(())
    }

    #[test]
    fn mcp_operations_back_up_and_copy_between_agents() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let ops = OperationManager::new(ctx.store.clone());
        let codex = adapter(AgentKind::Codex).mcp().unwrap().path(&ctx);
        fsx::atomic_write(&codex, b"model = \"o3\"\n", None)?;
        let record = ops.run(save_mcp_operation(
            &ctx,
            AgentKind::Codex,
            stdio("github"),
            None,
        )?)?;
        assert_eq!(record.state, OperationState::Succeeded);
        assert!(mcp_backup_dir(&ctx, AgentKind::Codex).read_dir()?.count() == 1);
        let listed = list(&ctx, AgentKind::Codex, None);
        assert_eq!(listed.mcp_servers, vec![stdio("github")]);
        assert!(listed.mcp_can_disable);

        let record = ops.run(set_mcp_enabled_operation(
            &ctx,
            AgentKind::Codex,
            "github".into(),
            false,
        )?)?;
        assert_eq!(record.state, OperationState::Succeeded);
        assert!(!list(&ctx, AgentKind::Codex, None).mcp_servers[0].enabled);
        assert!(
            set_mcp_enabled_operation(&ctx, AgentKind::Claude, "github".into(), false).is_err()
        );

        let record = ops.run(copy_mcp_operation(
            &ctx,
            AgentKind::Codex,
            "github".into(),
            vec![AgentKind::Claude, AgentKind::Opencode, AgentKind::Pi],
        )?)?;
        assert_eq!(record.state, OperationState::Succeeded);
        // The agent without a disable flag gets it switched on.
        assert!(list(&ctx, AgentKind::Claude, None).mcp_servers[0].enabled);
        assert!(!list(&ctx, AgentKind::Opencode, None).mcp_servers[0].enabled);
        assert!(
            copy_mcp_operation(
                &ctx,
                AgentKind::Codex,
                "github".into(),
                vec![AgentKind::Opencode]
            )
            .is_err()
        );

        let record = ops.run(remove_mcp_operation(
            &ctx,
            AgentKind::Codex,
            "github".into(),
        )?)?;
        assert_eq!(record.state, OperationState::Succeeded);
        assert!(list(&ctx, AgentKind::Codex, None).mcp_servers.is_empty());
        assert!(std::fs::read_to_string(&codex)?.contains("model = \"o3\""));
        assert!(save_mcp_operation(&ctx, AgentKind::Codex, stdio("bad name"), None).is_err());
        Ok(())
    }

    #[test]
    fn a_broken_mcp_file_is_reported_and_left_alone() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let ops = OperationManager::new(ctx.store.clone());
        let path = adapter(AgentKind::Opencode).mcp().unwrap().path(&ctx);
        fsx::atomic_write(&path, b"{ // comment\n}", None)?;
        assert!(list(&ctx, AgentKind::Opencode, None).mcp_error.is_some());
        let record = ops.run(save_mcp_operation(
            &ctx,
            AgentKind::Opencode,
            stdio("x"),
            None,
        )?)?;
        assert_eq!(record.state, OperationState::Failed);
        assert_eq!(std::fs::read_to_string(&path)?, "{ // comment\n}");
        Ok(())
    }

    #[test]
    fn skills_are_listed_copied_and_deleted_into_backups() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let ops = OperationManager::new(ctx.store.clone());
        let workspace = dir.path().join("w");
        let project = workspace
            .join(adapter(AgentKind::Pi).project_skills_dir())
            .join("local");
        fsx::atomic_write(
            &project.join("SKILL.md"),
            b"---\nname: local\ndescription: Project skill\n---\n",
            None,
        )?;

        let user_skill = user_skills_dir(&ctx, AgentKind::Pi).join("pdf-forms");
        fsx::atomic_write(
            &user_skill.join("SKILL.md"),
            b"---\nname: pdf-forms\ndescription: Fill PDF forms\n---\n\nUse pdftk.\n",
            None,
        )?;
        let listed = list(&ctx, AgentKind::Pi, Some(&workspace));
        let names: Vec<_> = listed
            .skills
            .iter()
            .map(|s| (s.name.as_str(), s.scope.as_str()))
            .collect();
        assert_eq!(names, vec![("pdf-forms", "user"), ("local", "project")]);
        assert_eq!(listed.skills[0].description, "Fill PDF forms");
        assert!(
            listed
                .mcp_path
                .as_deref()
                .is_some_and(|path| path.ends_with(".pi/agent/mcp.json"))
        );
        let skill = listed.skills[0].path.clone();
        std::fs::write(PathBuf::from(&skill).join("helper.sh"), "echo hi")?;

        let record = ops.run(copy_skill_operation(
            &ctx,
            AgentKind::Pi,
            skill.clone(),
            vec![AgentKind::Codex, AgentKind::Claude],
            None,
        )?)?;
        assert_eq!(record.state, OperationState::Succeeded);
        for kind in [AgentKind::Codex, AgentKind::Claude] {
            let copied = user_skills_dir(&ctx, kind).join("pdf-forms");
            assert!(copied.join("SKILL.md").is_file() && copied.join("helper.sh").is_file());
        }
        assert!(
            copy_skill_operation(
                &ctx,
                AgentKind::Pi,
                skill.clone(),
                vec![AgentKind::Codex],
                None
            )
            .is_err()
        );

        // Only paths inside the agent's own skill folders can be deleted.
        assert!(
            delete_skill_operation(&ctx, AgentKind::Pi, dir.path().display().to_string(), None)
                .is_err()
        );
        assert!(delete_skill_operation(&ctx, AgentKind::Codex, skill.clone(), None).is_err());
        let record = ops.run(delete_skill_operation(
            &ctx,
            AgentKind::Pi,
            skill.clone(),
            None,
        )?)?;
        assert_eq!(record.state, OperationState::Succeeded);
        assert!(!PathBuf::from(&skill).exists());
        let kept = skills_backup_dir(&ctx, AgentKind::Pi)
            .read_dir()?
            .next()
            .unwrap()?
            .path();
        assert!(kept.join("SKILL.md").is_file());
        let project_path = list(&ctx, AgentKind::Pi, Some(&workspace)).skills[0]
            .path
            .clone();
        let record = ops.run(delete_skill_operation(
            &ctx,
            AgentKind::Pi,
            project_path,
            Some(&workspace),
        )?)?;
        assert_eq!(record.state, OperationState::Succeeded);
        Ok(())
    }

    #[test]
    fn front_matter_reads_plain_quoted_and_folded_values() {
        let (name, description) =
            skill_front_matter("---\nname: pdf\ndescription: \"Fill PDF forms\"\n---\nbody");
        assert_eq!(name.as_deref(), Some("pdf"));
        assert_eq!(description.as_deref(), Some("Fill PDF forms"));
        let (_, description) = skill_front_matter(
            "---\nname: x\ndescription: >\n  Long text\n  over lines\nlicense: MIT\n---\n",
        );
        assert_eq!(description.as_deref(), Some("Long text over lines"));
        assert_eq!(skill_front_matter("no front matter"), (None, None));
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_skill_name("pdf-forms").is_ok());
        for bad in ["", "PDF", "a--b", "-a", "a b", "../x"] {
            assert!(valid_skill_name(bad).is_err(), "{bad}");
        }
        assert!(valid_mcp_name("github_2").is_ok());
        assert!(valid_mcp_name("a.b").is_err());
    }
}
