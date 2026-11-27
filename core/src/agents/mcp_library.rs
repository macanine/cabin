//! The user-level MCP catalog and its bindings to native agent configs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::adapter;
use super::extensions::{McpCodec, mcp_backup_dir, validate_server};
use crate::context::Context;
use crate::error::{Error, Result};
use crate::fsx;
use crate::model::{AgentKind, McpServer, McpTransport};
use crate::ops::{Operation, Step, StepCtx};

const SECRET: &str = "••••••••";
const MAX_BATCH: usize = 100;

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct McpLibrary {
    pub rows: Vec<McpRow>,
    pub agents: Vec<McpAgent>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct McpAgent {
    pub kind: AgentKind,
    pub path: Option<String>,
    pub error: Option<String>,
    pub supports_sse: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct McpRow {
    pub name: String,
    pub endpoint: String,
    pub transport: String,
    pub description: String,
    pub managed: bool,
    pub conflict: bool,
    pub bindings: Vec<McpBinding>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct McpBinding {
    pub kind: AgentKind,
    pub state: McpBindingState,
    pub wanted: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum McpBindingState {
    Enabled,
    Disabled,
    Unreadable,
    Conflict,
    Unsupported,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum McpChange {
    Import,
    Save {
        servers: Vec<McpServer>,
        targets: Vec<AgentKind>,
        replace: bool,
        description: String,
    },
    Bind {
        names: Vec<String>,
        targets: Vec<AgentKind>,
        enabled: bool,
    },
    Remove {
        names: Vec<String>,
    },
    Sync,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Managed {
    server: McpServer,
    targets: BTreeSet<AgentKind>,
    #[serde(default)]
    description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shelved: Option<Value>,
}

type Catalog = BTreeMap<String, Managed>;

fn catalog_path(ctx: &Context) -> PathBuf {
    ctx.paths.data().join("mcp-library.json")
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::path("read", path, error)),
    }
}

fn decode_catalog(text: Option<&str>) -> Result<Catalog> {
    text.map(serde_json::from_str)
        .transpose()
        .map(|value| value.unwrap_or_default())
        .map_err(|_| Error::invalid("The managed MCP catalog could not be read"))
}

struct Native {
    kind: AgentKind,
    codec: &'static dyn McpCodec,
    path: PathBuf,
    original: Option<String>,
    text: Option<String>,
    servers: BTreeMap<String, Option<McpServer>>,
}

fn decode_native(
    codec: &dyn McpCodec,
    text: Option<&str>,
) -> Result<BTreeMap<String, Option<McpServer>>> {
    let Some(text) = text else {
        return Ok(BTreeMap::new());
    };
    let mut servers: BTreeMap<_, _> = codec
        .ids(text)?
        .into_iter()
        .map(|name| (name, None))
        .collect();
    for server in codec.read(text)? {
        if validate_server(&server).is_ok() {
            let name = server.name.clone();
            servers.insert(name, Some(server));
        }
    }
    Ok(servers)
}

impl Native {
    fn read(kind: AgentKind, codec: &'static dyn McpCodec, path: PathBuf) -> Result<Self> {
        let text = read_optional(&path)?;
        let servers = decode_native(codec, text.as_deref()).map_err(|_| {
            Error::invalid(format!(
                "Could not read {} MCP config",
                adapter(kind).spec().name
            ))
        })?;
        Ok(Self {
            kind,
            codec,
            path,
            original: text.clone(),
            text,
            servers,
        })
    }

    fn has(&self, name: &str) -> Result<Option<&McpServer>> {
        match self.servers.get(name) {
            Some(Some(server)) => Ok(Some(server)),
            Some(None) => Err(Error::invalid(format!(
                "{} has an unreadable entry for {name}",
                adapter(self.kind).spec().name
            ))),
            None => Ok(None),
        }
    }

    fn update(&mut self, text: String) -> Result<()> {
        self.servers = decode_native(self.codec, Some(&text))?;
        self.text = Some(text);
        Ok(())
    }

    fn remove(&mut self, name: &str) -> Result<()> {
        if self.has(name)?.is_some() {
            self.update(
                self.codec
                    .remove(self.text.as_deref().unwrap_or_default(), name)?,
            )?;
        }
        Ok(())
    }

    fn bind(
        &mut self,
        entry: &mut Managed,
        previous: Option<&McpServer>,
        enabled: bool,
    ) -> Result<()> {
        let name = &entry.server.name;
        let existing = self.has(name)?;
        if let Some(current) = existing {
            if !equivalent(self.kind, current, &entry.server)
                && !previous.is_some_and(|old| equivalent(self.kind, current, old))
            {
                return Err(Error::Conflict(format!(
                    "{} has a different configuration for {name}",
                    adapter(self.kind).spec().name
                )));
            }
            if current.enabled == enabled && equivalent(self.kind, current, &entry.server) {
                return Ok(());
            }
        } else if !enabled {
            return Ok(());
        }
        if matches!(entry.server.transport, McpTransport::Http { sse: true, .. })
            && !self.codec.supports_sse()
        {
            if enabled {
                return Err(Error::invalid(format!(
                    "{} requires Streamable HTTP for remote MCP servers",
                    adapter(self.kind).spec().name
                )));
            }
            return self.remove(name);
        }
        let text = self.text.as_deref();
        if !enabled && !self.codec.can_disable() {
            let current = text.unwrap_or_default();
            entry.shelved = self.codec.shelve(current, name)?;
            return self.remove(name);
        }
        let restored;
        let text = if self.codec.can_disable() {
            text
        } else if let Some(shelved) = entry.shelved.take().filter(|_| existing.is_none()) {
            restored = self.codec.unshelve(text, name, &shelved)?;
            Some(restored.as_str())
        } else {
            text
        };
        let mut server = entry.server.clone();
        server.enabled = enabled;
        self.update(self.codec.upsert(text, &server, None)?)
    }
}

struct AgentFile {
    kind: AgentKind,
    path: Option<PathBuf>,
    native: Result<Option<Native>>,
}

fn native_files(ctx: &Context) -> Vec<AgentFile> {
    AgentKind::ALL
        .into_iter()
        .map(|kind| {
            let codec = adapter(kind).mcp();
            let path = codec.map(|codec| codec.path(ctx));
            let native = codec
                .zip(path.clone())
                .map(|(codec, path)| Native::read(kind, codec, path))
                .transpose();
            AgentFile { kind, path, native }
        })
        .collect()
}

fn equivalent(kind: AgentKind, current: &McpServer, expected: &McpServer) -> bool {
    if current.transport == expected.transport {
        return true;
    }
    // OpenCode negotiates the remote protocol and has a single native remote type.
    matches!((&current.transport, &expected.transport),
        (McpTransport::Http { url, headers, .. }, McpTransport::Http { url: other_url, headers: other_headers, .. })
        if kind == AgentKind::Opencode && url == other_url && headers == other_headers)
}

fn inspected<'a>(
    files: &'a [AgentFile],
    name: &str,
) -> impl Iterator<Item = (AgentKind, &'a McpServer)> {
    files.iter().filter_map(move |file| {
        let server = file
            .native
            .as_ref()
            .ok()?
            .as_ref()?
            .servers
            .get(name)?
            .as_ref()?;
        Some((file.kind, server))
    })
}

fn names(catalog: &Catalog, files: &[AgentFile]) -> BTreeSet<String> {
    catalog
        .keys()
        .chain(
            files
                .iter()
                .filter_map(|file| file.native.as_ref().ok()?.as_ref())
                .flat_map(|file| file.servers.keys()),
        )
        .cloned()
        .collect()
}

fn row(catalog: &Catalog, files: &[AgentFile], name: String) -> McpRow {
    let entry = catalog.get(&name);
    let first = inspected(files, &name).next();
    let server = entry
        .map(|entry| &entry.server)
        .or_else(|| first.map(|(_, server)| server));
    let bindings: Vec<_> = files
        .iter()
        .map(|file| {
            let state = match &file.native {
                Ok(Some(native)) => match native.servers.get(&name) {
                    Some(Some(current))
                        if server.is_some_and(|server| !equivalent(file.kind, current, server)) =>
                    {
                        McpBindingState::Conflict
                    }
                    Some(Some(current)) if current.enabled => McpBindingState::Enabled,
                    None if !native.codec.supports_sse()
                        && server.is_some_and(|server| {
                            matches!(server.transport, McpTransport::Http { sse: true, .. })
                        }) =>
                    {
                        McpBindingState::Unsupported
                    }
                    Some(Some(_)) | None => McpBindingState::Disabled,
                    Some(None) => McpBindingState::Unreadable,
                },
                Ok(None) => McpBindingState::Unsupported,
                Err(_) => McpBindingState::Unreadable,
            };
            McpBinding {
                kind: file.kind,
                state,
                wanted: entry.is_some_and(|entry| entry.targets.contains(&file.kind)),
            }
        })
        .collect();
    let (endpoint, transport) = match server.map(|server| &server.transport) {
        Some(McpTransport::Stdio { command, .. }) => (command.clone(), "stdio"),
        Some(McpTransport::Http { url, sse, .. }) => {
            let endpoint = url::Url::parse(url)
                .map(|mut url| {
                    let _ = url.set_username("");
                    let _ = url.set_password(None);
                    url.set_query(None);
                    url.set_fragment(None);
                    url.to_string()
                })
                .unwrap_or_default();
            (endpoint, if *sse { "SSE" } else { "HTTP" })
        }
        None => (String::new(), ""),
    };
    let conflict = bindings
        .iter()
        .any(|binding| matches!(binding.state, McpBindingState::Conflict));
    McpRow {
        name,
        endpoint,
        transport: transport.into(),
        description: entry.map_or_else(String::new, |entry| entry.description.clone()),
        managed: entry.is_some(),
        conflict,
        bindings,
    }
}

pub fn list(ctx: &Context) -> Result<McpLibrary> {
    let catalog = decode_catalog(read_optional(&catalog_path(ctx))?.as_deref())?;
    let files = native_files(ctx);
    let rows = names(&catalog, &files)
        .into_iter()
        .map(|name| row(&catalog, &files, name))
        .collect();
    let agents = files
        .into_iter()
        .map(|file| McpAgent {
            kind: file.kind,
            path: file.path.map(|path| path.display().to_string()),
            error: file.native.err().map(|error| error.to_string()),
            supports_sse: adapter(file.kind).mcp().is_some_and(McpCodec::supports_sse),
        })
        .collect();
    Ok(McpLibrary { rows, agents })
}

fn secrets(transport: &mut McpTransport) -> &mut BTreeMap<String, String> {
    match transport {
        McpTransport::Stdio { env, .. } => env,
        McpTransport::Http { headers, .. } => headers,
    }
}

pub fn edit(ctx: &Context, name: &str) -> Result<McpServer> {
    let catalog = decode_catalog(read_optional(&catalog_path(ctx))?.as_deref())?;
    let mut server = catalog
        .get(name)
        .ok_or_else(|| Error::NotFound(format!("Managed MCP server {name}")))?
        .server
        .clone();
    for value in secrets(&mut server.transport).values_mut() {
        *value = SECRET.into();
    }
    Ok(server)
}

fn restore_secrets(server: &mut McpServer, previous: Option<&McpServer>) -> Result<()> {
    let previous = previous.filter(|previous| {
        std::mem::discriminant(&previous.transport) == std::mem::discriminant(&server.transport)
    });
    let old = previous.map(|server| {
        let mut transport = server.transport.clone();
        std::mem::take(secrets(&mut transport))
    });
    for (key, value) in secrets(&mut server.transport) {
        if value == SECRET {
            *value = old
                .as_ref()
                .and_then(|map| map.get(key))
                .cloned()
                .ok_or_else(|| Error::invalid(format!("Enter a value for {key}")))?;
        }
    }
    Ok(())
}

pub fn export(ctx: &Context, name: Option<&str>) -> Result<String> {
    let catalog = decode_catalog(read_optional(&catalog_path(ctx))?.as_deref())?;
    let mut servers = serde_json::Map::new();
    for (id, entry) in catalog {
        if name.is_some_and(|name| name != id) {
            continue;
        }
        let mut transport = serde_json::to_value(entry.server.transport)?;
        if transport["type"] == "http" {
            let sse = transport
                .as_object_mut()
                .and_then(|map| map.shift_remove("sse"));
            if sse == Some(Value::Bool(true)) {
                transport["type"] = "sse".into();
            }
        }
        servers.insert(id, transport);
    }
    if let Some(name) = name.filter(|_| servers.is_empty()) {
        return Err(Error::NotFound(format!("Managed MCP server {name}")));
    }
    serde_json::to_string_pretty(&serde_json::json!({"mcpServers": servers})).map_err(Into::into)
}

fn require_codec(kind: AgentKind) -> Result<&'static dyn McpCodec> {
    let agent = adapter(kind);
    agent
        .mcp()
        .ok_or_else(|| Error::Unavailable(format!("{} has no MCP support", agent.spec().name)))
}

fn decode_pasted_json(mut value: Value) -> Result<BTreeMap<String, Option<McpServer>>> {
    let entries = super::extensions::json::table(&mut value, "mcpServers")?;
    let mut disabled = BTreeSet::new();
    for (name, entry) in entries.iter_mut() {
        let Some(entry) = entry.as_object_mut() else {
            continue;
        };
        // Antigravity's MCP config uses `serverUrl` and `disabled`.
        if let Some(url) = entry.shift_remove("serverUrl") {
            entry.insert("url".into(), url);
        }
        if entry.get("disabled").and_then(Value::as_bool) == Some(true) {
            disabled.insert(name.clone());
        }
    }
    let codec = require_codec(AgentKind::Claude)?;
    let mut servers = decode_native(codec, Some(&value.to_string()))?;
    for name in disabled {
        if let Some(Some(server)) = servers.get_mut(&name) {
            server.enabled = false;
        }
    }
    Ok(servers)
}

pub fn parse_paste(text: &str) -> Result<Vec<McpServer>> {
    if text.len() > 2 * 1024 * 1024 {
        return Err(Error::invalid("Paste a config smaller than 2 MiB"));
    }
    let decode = |agent| decode_native(require_codec(agent)?, Some(text));
    let servers = match serde_json::from_str::<Value>(text) {
        Ok(value) if value.get("mcp").is_some() => decode(AgentKind::Opencode),
        Ok(value) if value.get("mcpServers").is_some() => decode_pasted_json(value),
        Ok(mut value) => {
            let name = value
                .as_object_mut()
                .and_then(|map| map.shift_remove("name"))
                .and_then(|name| name.as_str().map(str::to_owned))
                .ok_or_else(|| Error::invalid("Provide a named server or an mcpServers object"))?;
            if value.get("transport").is_some() {
                let server = serde_json::from_value::<McpServer>(serde_json::json!({
                    "name": name, "transport": value["transport"],
                    "enabled": value.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                }))
                .map_err(|_| Error::invalid("Invalid MCP server fields"))?;
                validate_server(&server)?;
                return Ok(vec![server]);
            }
            decode_pasted_json(serde_json::json!({"mcpServers": {name: value}}))
        }
        Err(_) => decode(AgentKind::Codex),
    }
    .map_err(|_| Error::invalid("Paste valid MCP JSON or Codex TOML"))?;
    if servers.is_empty() || servers.len() > MAX_BATCH {
        return Err(Error::invalid(format!(
            "Paste between 1 and {MAX_BATCH} MCP servers"
        )));
    }
    servers
        .into_iter()
        .map(|(name, server)| {
            server.ok_or_else(|| Error::invalid(format!("Could not read MCP server {name}")))
        })
        .collect()
}

fn import(catalog: &mut Catalog, files: &[AgentFile]) {
    for name in names(catalog, files) {
        let mut native = inspected(files, &name);
        let Some((first_kind, first)) = native.next() else {
            continue;
        };
        let matches = native.all(|(kind, current)| equivalent(kind, current, first));
        drop(native);
        if !matches
            || !equivalent(
                first_kind,
                first,
                catalog.get(&name).map_or(first, |entry| &entry.server),
            )
        {
            continue;
        }
        let targets: BTreeSet<_> = inspected(files, &name)
            .filter(|(_, server)| server.enabled)
            .map(|(kind, _)| kind)
            .collect();
        catalog
            .entry(name)
            .and_modify(|entry| entry.targets.extend(targets.iter().copied()))
            .or_insert_with(|| Managed {
                server: first.clone(),
                targets: targets.clone(),
                description: String::new(),
                shelved: None,
            });
    }
}

fn set_bindings(
    files: &mut [AgentFile],
    entry: &mut Managed,
    previous: Option<&McpServer>,
    targets: &BTreeSet<AgentKind>,
) -> Result<()> {
    for file in files {
        let enabled = targets.contains(&file.kind);
        let was_enabled = entry.targets.contains(&file.kind);
        let existing = file
            .native
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .is_some_and(|native| native.servers.contains_key(&entry.server.name));
        if !enabled && !was_enabled && !existing {
            continue;
        }
        let native = file
            .native
            .as_mut()
            .map_err(|error| Error::invalid(error.to_string()))?
            .as_mut()
            .ok_or_else(|| {
                Error::invalid(format!(
                    "{} has no MCP support",
                    adapter(file.kind).spec().name
                ))
            })?;
        native.bind(entry, previous, enabled)?;
    }
    entry.targets.clone_from(targets);
    Ok(())
}

fn apply(change: McpChange, catalog: &mut Catalog, files: &mut [AgentFile]) -> Result<()> {
    match change {
        McpChange::Import => import(catalog, files),
        McpChange::Save {
            servers,
            targets,
            replace,
            description,
        } => {
            if servers.is_empty() || servers.len() > MAX_BATCH || description.len() > 1000 {
                return Err(Error::invalid("The MCP batch or description is too large"));
            }
            let targets = targets.into_iter().collect();
            let mut seen = BTreeSet::new();
            for mut server in servers {
                validate_server(&server)?;
                let name = server.name.clone();
                if !seen.insert(name.clone()) {
                    return Err(Error::Conflict(format!("Duplicate MCP server {name}")));
                }
                let old = catalog.get(&name);
                let existing = inspected(files, &name).next();
                let unreadable = files.iter().any(|file| {
                    file.native
                        .as_ref()
                        .ok()
                        .and_then(Option::as_ref)
                        .is_some_and(|native| native.servers.get(&name) == Some(&None))
                });
                if !replace && (old.is_some() || existing.is_some() || unreadable) {
                    continue;
                }
                if unreadable {
                    return Err(Error::invalid(format!(
                        "An agent has an unreadable entry for {name}"
                    )));
                }
                let previous = old
                    .map(|entry| &entry.server)
                    .or_else(|| existing.map(|(_, server)| server))
                    .cloned();
                // An explicit replacement can change a single agreed native definition.
                if let Some(previous) = &previous {
                    for (kind, current) in inspected(files, &name) {
                        if !equivalent(kind, current, previous) {
                            return Err(Error::Conflict(format!(
                                "Resolve the different native configurations for {name} before replacing it"
                            )));
                        }
                    }
                }
                restore_secrets(&mut server, previous.as_ref())?;
                let mut entry = Managed {
                    server,
                    targets: old.map(|entry| entry.targets.clone()).unwrap_or_default(),
                    description: description.clone(),
                    shelved: old.and_then(|entry| entry.shelved.clone()),
                };
                if let Some(shelved) = &entry.shelved {
                    let codec = require_codec(AgentKind::Claude)?;
                    let text = codec.unshelve(None, &name, shelved)?;
                    let text = codec.upsert(Some(&text), &entry.server, None)?;
                    entry.shelved = codec.shelve(&text, &name)?;
                }
                let wanted = if entry.server.enabled {
                    &targets
                } else {
                    &BTreeSet::new()
                };
                set_bindings(files, &mut entry, previous.as_ref(), wanted)?;
                catalog.insert(name, entry);
            }
        }
        McpChange::Bind {
            names,
            targets,
            enabled,
        } => {
            for name in names.into_iter().collect::<BTreeSet<_>>() {
                let entry = catalog
                    .get_mut(&name)
                    .ok_or_else(|| Error::NotFound(format!("Managed MCP server {name}")))?;
                for file in files.iter_mut().filter(|file| targets.contains(&file.kind)) {
                    let native = file
                        .native
                        .as_mut()
                        .map_err(|error| Error::invalid(error.to_string()))?
                        .as_mut()
                        .ok_or_else(|| {
                            Error::invalid(format!(
                                "{} has no MCP support",
                                adapter(file.kind).spec().name
                            ))
                        })?;
                    native.bind(entry, None, enabled)?;
                    if enabled {
                        entry.targets.insert(file.kind);
                    } else {
                        entry.targets.remove(&file.kind);
                    }
                }
            }
        }
        McpChange::Remove { names } => {
            for name in names.into_iter().collect::<BTreeSet<_>>() {
                let entry = catalog
                    .remove(&name)
                    .ok_or_else(|| Error::NotFound(format!("Managed MCP server {name}")))?;
                for file in files.iter_mut() {
                    let native = match &mut file.native {
                        Ok(Some(native)) => native,
                        Err(error) if entry.targets.contains(&file.kind) => {
                            return Err(Error::invalid(error.to_string()));
                        }
                        _ => continue,
                    };
                    if native
                        .has(&name)?
                        .is_some_and(|server| !equivalent(native.kind, server, &entry.server))
                    {
                        return Err(Error::Conflict(format!(
                            "{} has a different configuration for {name}",
                            adapter(file.kind).spec().name
                        )));
                    }
                    native.remove(&name)?;
                }
            }
        }
        McpChange::Sync => {
            for entry in catalog.values_mut() {
                let targets = entry.targets.clone();
                set_bindings(files, entry, None, &targets)?;
            }
        }
    }
    Ok(())
}

struct FileChange {
    path: PathBuf,
    original: Option<String>,
    text: String,
    applied: bool,
}

impl FileChange {
    fn new(path: PathBuf, original: Option<String>, text: String) -> Self {
        Self {
            path,
            original,
            text,
            applied: false,
        }
    }
}

struct WriteMcp {
    ctx: Context,
    change: Option<McpChange>,
    writes: Vec<FileChange>,
}

impl Step for WriteMcp {
    fn name(&self) -> String {
        "Update MCP library and agent configs".into()
    }

    fn prepare(&mut self, step: &StepCtx<'_>) -> Result<()> {
        let path = catalog_path(&self.ctx);
        let original = read_optional(&path)?;
        let baseline = decode_catalog(original.as_deref())?;
        let mut catalog = baseline.clone();
        let mut files = native_files(&self.ctx);
        let change = self
            .change
            .take()
            .ok_or_else(|| Error::Internal("The MCP operation was already prepared".into()))?;
        apply(change, &mut catalog, &mut files)?;
        for native in files
            .into_iter()
            .filter_map(|file| file.native.ok().flatten())
        {
            if native.text == native.original {
                continue;
            }
            if let Some(backup) =
                fsx::backup(&native.path, &mcp_backup_dir(&self.ctx, native.kind))?
            {
                step.log(format!("Backed up to {}", backup.display()));
            }
            let text = native.text.ok_or_else(|| {
                Error::Internal(format!(
                    "MCP update for {} has no contents",
                    native.path.display()
                ))
            })?;
            self.writes
                .push(FileChange::new(native.path, native.original, text));
        }
        if catalog != baseline {
            self.writes.push(FileChange::new(
                path,
                original,
                serde_json::to_string_pretty(&catalog)?,
            ));
        }
        Ok(())
    }

    fn apply(&mut self, step: &StepCtx<'_>) -> Result<()> {
        for write in &mut self.writes {
            if step.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if read_optional(&write.path)? != write.original {
                return Err(Error::Conflict(format!(
                    "{} changed during the operation",
                    write.path.display()
                )));
            }
            let mode = (write.original.is_none() || write.path == catalog_path(&self.ctx))
                .then_some(0o600);
            fsx::atomic_write(&write.path, write.text.as_bytes(), mode)?;
            write.applied = true;
            step.log(format!("Updated {}", write.path.display()));
        }
        Ok(())
    }

    fn verify(&mut self, _: &StepCtx<'_>) -> Result<()> {
        for write in &self.writes {
            if read_optional(&write.path)?.as_deref() != Some(&write.text) {
                return Err(Error::Conflict(format!(
                    "{} changed after writing",
                    write.path.display()
                )));
            }
        }
        Ok(())
    }

    fn rollback(&mut self, _: &StepCtx<'_>) -> Result<()> {
        let mut failure = None;
        for write in self.writes.iter().rev().filter(|write| write.applied) {
            let restore = (|| {
                if read_optional(&write.path)?.as_deref() != Some(&write.text) {
                    return Err(Error::Conflict(format!(
                        "{} changed again before rollback",
                        write.path.display()
                    )));
                }
                match &write.original {
                    Some(text) => fsx::atomic_write(&write.path, text.as_bytes(), None),
                    None => fsx::restore(&write.path, None),
                }
            })();
            if let Err(error) = restore {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

pub fn operation(ctx: &Context, change: McpChange) -> Operation {
    let title = match change {
        McpChange::Import => "Import MCP servers from agents",
        McpChange::Save { .. } => "Save MCP servers",
        McpChange::Bind { enabled: true, .. } => "Enable MCP servers",
        McpChange::Bind { enabled: false, .. } => "Disable MCP servers",
        McpChange::Remove { .. } => "Delete MCP servers",
        McpChange::Sync => "Sync MCP servers to agents",
    };
    Operation::new("agent.mcpLibrary", title)
        .key("mcp-library")
        .step(WriteMcp {
            ctx: ctx.clone(),
            change: Some(change),
            writes: Vec::new(),
        })
}
