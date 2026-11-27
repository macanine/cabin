use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::{AgentAdapter, AgentSpec, LaunchOptions, find_binary, positional};
use crate::context::Context;
use crate::environment;
use crate::error::{Error, Result};
use crate::fsx;
use crate::model::{
    AgentKind, CodexAccount, CodexAccounts, CodexCredits, CodexQuota, CodexQuotaWindow,
    CodexResetCard, CodexResetResult, SkillEntry,
};
use crate::system::CommandSpec;
use crate::util::now_millis;

pub struct Codex;

static SPEC: AgentSpec = AgentSpec {
    kind: AgentKind::Codex,
    name: "Codex",
    binary: "codex",
    package: "@openai/codex",
    config: ".codex/config.toml",
    model_key: "model",
    auth_files: &[".codex/auth.json"],
    auth_env: &["OPENAI_API_KEY"],
};

impl AgentAdapter for Codex {
    fn skill_insert(&self, skill: &SkillEntry) -> Option<String> {
        Some(format!("${} ", skill.name))
    }

    fn spec(&self) -> &'static AgentSpec {
        &SPEC
    }

    fn launch_args(&self, options: &LaunchOptions) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(model) = options.model.as_deref().filter(|m| !m.is_empty()) {
            args.extend(["--model".into(), model.into()]);
        }
        if options.resume {
            args.extend(["resume".into(), "--last".into()]);
        } else if let Some(prompt) = options
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            args.push(positional(prompt));
        }
        args
    }

    fn mcp(&self) -> Option<&'static dyn super::extensions::McpCodec> {
        Some(&Mcp)
    }

    fn skills_dir(&self, ctx: &Context) -> PathBuf {
        codex_home(ctx).join("skills")
    }

    fn project_skills_dir(&self) -> &'static str {
        ".codex/skills"
    }
}

/// `[mcp_servers.<name>]` in config.toml: `command`, `args`, `env` for a
/// local server, `url` and `http_headers` for a remote one, `enabled`
/// (default true). Edited with toml_edit so comments and layout survive.
struct Mcp;

fn toml_strings(
    item: Option<&toml_edit::Item>,
) -> Option<std::collections::BTreeMap<String, String>> {
    match item {
        Some(item) => item
            .as_table_like()?
            .iter()
            .map(|(key, value)| Some((key.to_owned(), value.as_str()?.to_owned())))
            .collect(),
        None => Some(Default::default()),
    }
}

fn toml_inline(map: &std::collections::BTreeMap<String, String>) -> toml_edit::Item {
    let mut table = toml_edit::InlineTable::new();
    for (key, value) in map {
        table.insert(key, value.as_str().into());
    }
    toml_edit::value(table)
}

fn toml_doc(text: Option<&str>) -> Result<toml_edit::DocumentMut> {
    text.unwrap_or("")
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| {
            Error::invalid(format!(
                "config.toml is not valid TOML ({error}); fix it by hand first"
            ))
        })
}

impl super::extensions::McpCodec for Mcp {
    fn path(&self, ctx: &Context) -> PathBuf {
        codex_home(ctx).join("config.toml")
    }

    fn can_disable(&self) -> bool {
        true
    }

    fn supports_sse(&self) -> bool {
        false
    }

    fn ids(&self, text: &str) -> Result<Vec<String>> {
        let doc = toml_doc(Some(text))?;
        let Some(item) = doc.get("mcp_servers") else {
            return Ok(Vec::new());
        };
        let table = item
            .as_table_like()
            .ok_or_else(|| Error::invalid("`mcp_servers` in config.toml is not a table"))?;
        Ok(table.iter().map(|(name, _)| name.to_owned()).collect())
    }

    fn read(&self, text: &str) -> Result<Vec<crate::model::McpServer>> {
        use crate::model::{McpServer, McpTransport};
        let doc = toml_doc(Some(text))?;
        let Some(servers) = doc
            .get("mcp_servers")
            .and_then(toml_edit::Item::as_table_like)
        else {
            return Ok(Vec::new());
        };
        Ok(servers
            .iter()
            .filter_map(|(name, item)| {
                let entry = item.as_table_like()?;
                let transport = match entry.get("url") {
                    Some(url) => McpTransport::Http {
                        url: url.as_str()?.into(),
                        headers: toml_strings(entry.get("http_headers"))?,
                        sse: false,
                    },
                    None => McpTransport::Stdio {
                        command: entry.get("command")?.as_str()?.into(),
                        args: match entry.get("args") {
                            Some(args) => args
                                .as_array()?
                                .iter()
                                .map(|value| value.as_str().map(str::to_owned))
                                .collect::<Option<_>>()?,
                            None => Vec::new(),
                        },
                        env: toml_strings(entry.get("env"))?,
                    },
                };
                let enabled =
                    entry.get("enabled").and_then(toml_edit::Item::as_bool) != Some(false);
                Some(McpServer {
                    name: name.to_string(),
                    transport,
                    enabled,
                })
            })
            .collect())
    }

    fn upsert(
        &self,
        text: Option<&str>,
        server: &crate::model::McpServer,
        previous: Option<&str>,
    ) -> Result<String> {
        use crate::model::McpTransport;
        let mut doc = toml_doc(text)?;
        if doc.get("mcp_servers").is_none() {
            let mut table = toml_edit::Table::new();
            table.set_implicit(true);
            doc.insert("mcp_servers", toml_edit::Item::Table(table));
        }
        let servers = doc["mcp_servers"]
            .as_table_like_mut()
            .ok_or_else(|| Error::invalid("`mcp_servers` in config.toml is not a table"))?;
        let old = previous.unwrap_or(&server.name);
        if old != server.name && servers.contains_key(&server.name) {
            return Err(Error::Conflict(format!(
                "An MCP server named {} already exists",
                server.name
            )));
        }
        let mut entry = servers
            .remove(old)
            .filter(|item| item.is_table_like())
            .unwrap_or_else(|| toml_edit::Item::Table(toml_edit::Table::new()));
        let table = entry.as_table_like_mut().ok_or_else(|| {
            Error::invalid(format!("The MCP entry {old} in config.toml is not a table"))
        })?;
        match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                for key in ["url", "http_headers", "bearer_token_env_var"] {
                    table.remove(key);
                }
                table.insert("command", toml_edit::value(command.as_str()));
                let mut list = toml_edit::Array::new();
                for arg in args {
                    list.push(arg.as_str());
                }
                table.insert("args", toml_edit::value(list));
                if env.is_empty() {
                    table.remove("env");
                } else {
                    table.insert("env", toml_inline(env));
                }
            }
            McpTransport::Http { url, headers, sse } => {
                if *sse {
                    return Err(Error::invalid(
                        "Codex requires streamable HTTP for remote MCP servers",
                    ));
                }
                for key in ["command", "args", "env", "cwd"] {
                    table.remove(key);
                }
                table.insert("url", toml_edit::value(url.as_str()));
                if headers.is_empty() {
                    table.remove("http_headers");
                } else {
                    table.insert("http_headers", toml_inline(headers));
                }
            }
        }
        if server.enabled {
            table.remove("enabled");
        } else {
            table.insert("enabled", toml_edit::value(false));
        }
        servers.insert(&server.name, entry);
        Ok(doc.to_string())
    }

    fn remove(&self, text: &str, name: &str) -> Result<String> {
        let mut doc = toml_doc(Some(text))?;
        let removed = doc
            .get_mut("mcp_servers")
            .and_then(toml_edit::Item::as_table_like_mut)
            .and_then(|servers| servers.remove(name));
        if removed.is_none() {
            return Err(Error::NotFound(format!("MCP server {name}")));
        }
        Ok(doc.to_string())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountIndex {
    #[serde(default)]
    active_account_id: Option<String>,
    #[serde(default)]
    accounts: Vec<StoredAccount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredAccount {
    id: String,
    fingerprint: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    plan_type: Option<String>,
    auth_mode: String,
    #[serde(default)]
    subscription_until: Option<String>,
    imported_at: i64,
    #[serde(default)]
    last_used_at: Option<i64>,
    #[serde(default)]
    quota: Option<CodexQuota>,
}

#[derive(Clone)]
struct ParsedAuth {
    bytes: Vec<u8>,
    fingerprint: String,
    email: Option<String>,
    plan_type: Option<String>,
    auth_mode: String,
    subscription_until: Option<String>,
}

fn codex_home(ctx: &Context) -> PathBuf {
    #[cfg(not(test))]
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(home);
    }
    ctx.paths.home().join(".codex")
}

fn auth_path(ctx: &Context) -> PathBuf {
    codex_home(ctx).join("auth.json")
}

fn accounts_root(ctx: &Context) -> PathBuf {
    ctx.paths.data().join("codex-accounts")
}

fn index_path(ctx: &Context) -> PathBuf {
    accounts_root(ctx).join("accounts.json")
}

fn account_path(ctx: &Context, id: &str) -> PathBuf {
    accounts_root(ctx)
        .join("credentials")
        .join(format!("{id}.json"))
}

fn check_file_store(ctx: &Context) -> Result<()> {
    let path = codex_home(ctx).join("config.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Error::path("read", &path, error)),
    };
    let doc = text.parse::<toml_edit::DocumentMut>().map_err(|error| {
        Error::invalid(format!("{} is not valid TOML ({error})", path.display()))
    })?;
    let mode = doc
        .get("cli_auth_credentials_store")
        .and_then(|item| item.as_str());
    if matches!(mode, Some("keyring" | "ephemeral")) {
        return Err(Error::Conflict(format!(
            "Codex uses {mode:?} credential storage in {}; choose file storage before switching imported credentials",
            path.display()
        )));
    }
    Ok(())
}

fn jwt_payload(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?.trim_end_matches('=');
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn safe_claim(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?.trim();
    (!value.is_empty() && value.len() <= 320 && !value.chars().any(char::is_control))
        .then(|| value.to_string())
}

fn fingerprint(auth_mode: &str, account_id: &str, email: &str, plan: &str, bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"cabin codex account\0");
    digest.update(auth_mode.as_bytes());
    digest.update([0]);
    digest.update(account_id.as_bytes());
    digest.update([0]);
    digest.update(email.as_bytes());
    digest.update([0]);
    digest.update(plan.as_bytes());
    if account_id.is_empty() && auth_mode == "apikey" {
        digest.update([0]);
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

fn parse_auth(text: &str) -> Result<ParsedAuth> {
    parse_auth_with_hints(text, AuthHints::default())
}

#[derive(Debug, Default)]
struct AuthHints {
    email: Option<String>,
    account_id: Option<String>,
    plan_type: Option<String>,
}

fn parse_auth_with_hints(text: &str, hints: AuthHints) -> Result<ParsedAuth> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        Error::invalid(format!("The Codex auth file is not valid JSON: {error}"))
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| Error::invalid("The Codex auth file is not a JSON object"))?;
    let tokens = object.get("tokens").and_then(Value::as_object);
    let access_token = tokens
        .and_then(|tokens| tokens.get("access_token"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let id_token = tokens
        .and_then(|tokens| tokens.get("id_token"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let api_key = object
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .unwrap_or("");
    let auth_mode = object
        .get("auth_mode")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            if !access_token.is_empty() {
                "chatgpt".into()
            } else if !api_key.is_empty() {
                "apikey".into()
            } else {
                "unknown".into()
            }
        });
    if auth_mode == "apikey" {
        if api_key.trim().is_empty() {
            return Err(Error::invalid("The Codex auth file has no OPENAI_API_KEY"));
        }
    } else if access_token.trim().is_empty() {
        return Err(Error::invalid(
            "The Codex auth file has no ChatGPT access token",
        ));
    }

    let claims = jwt_payload(if id_token.is_empty() {
        access_token
    } else {
        id_token
    })
    .unwrap_or(Value::Null);
    let auth_claims = claims.get("https://api.openai.com/auth");
    let email = safe_claim(claims.get("email"));
    let account_id = tokens
        .and_then(|tokens| tokens.get("account_id"))
        .and_then(Value::as_str)
        .or_else(|| auth_claims?.get("chatgpt_account_id")?.as_str())
        .or_else(|| object.get("account_id").and_then(Value::as_str))
        .or_else(|| object.get("chatgpt_account_id").and_then(Value::as_str))
        .or(hints.account_id.as_deref())
        .unwrap_or("");
    let plan_type = safe_claim(auth_claims.and_then(|claims| claims.get("chatgpt_plan_type")))
        .or_else(|| safe_claim(object.get("plan_type")))
        .or(hints.plan_type);
    let subscription_until =
        safe_claim(auth_claims.and_then(|claims| claims.get("chatgpt_subscription_active_until")));
    let email = email
        .or_else(|| safe_claim(object.get("email")))
        .or(hints.email);
    let bytes = text.trim().as_bytes().to_vec();
    if bytes.is_empty() {
        return Err(Error::invalid("The Codex auth file is empty"));
    }
    Ok(ParsedAuth {
        fingerprint: fingerprint(
            &auth_mode,
            account_id,
            email.as_deref().unwrap_or(""),
            plan_type.as_deref().unwrap_or(""),
            &bytes,
        ),
        bytes,
        email,
        plan_type,
        auth_mode,
        subscription_until,
    })
}

fn field<'a>(object: &'a Map<String, Value>, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_str))
}

fn token_field(object: &Map<String, Value>, names: &[&str], label: &str) -> Result<String> {
    let token = field(object, names).map(str::trim).unwrap_or("");
    if token.is_empty() {
        return Err(Error::invalid(format!(
            "The imported Codex account has no {label}"
        )));
    }
    if token
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(Error::invalid(format!(
            "The imported Codex account has an invalid {label}"
        )));
    }
    Ok(token.to_string())
}

fn chatgpt_auth_from_export(object: &Map<String, Value>, format: &str) -> Result<ParsedAuth> {
    let access_token = token_field(object, &["access_token", "accessToken"], "access_token")?;
    let refresh_token = token_field(object, &["refresh_token", "refreshToken"], "refresh_token")
        .map_err(|error| {
            Error::invalid(format!(
                "{format} account cannot be renewed without a refresh_token: {error}"
            ))
        })?;
    let id_token = field(object, &["id_token", "idToken"])
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let account_id = field(object, &["account_id", "accountId", "chatgpt_account_id"])
        .and_then(|value| safe_claim(Some(&Value::String(value.to_string()))))
        .unwrap_or_default();
    let email = field(object, &["email", "email_address"])
        .and_then(|value| safe_claim(Some(&Value::String(value.to_string()))));
    let plan_type = field(object, &["plan_type", "planType"])
        .and_then(|value| safe_claim(Some(&Value::String(value.to_string()))));
    // Keep the export's identity hints with the native credentials. Otherwise
    // a token without those claims gets a different fingerprint when re-read.
    let native = json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "email": email,
        "plan_type": plan_type,
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": account_id,
        }
    });
    parse_auth(&serde_json::to_string(&native)?)
}

fn collect_import_auths(value: &Value, output: &mut Vec<ParsedAuth>) -> Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_import_auths(item, output)?;
            }
        }
        Value::Object(object) => {
            if object.contains_key("tokens")
                || object.contains_key("auth_mode")
                || object.contains_key("OPENAI_API_KEY")
            {
                output.push(parse_auth(&serde_json::to_string(value)?)?);
                return Ok(());
            }

            if object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("sub2api-data"))
            {
                let accounts = object
                    .get("accounts")
                    .and_then(Value::as_array)
                    .ok_or_else(|| Error::invalid("The Sub2API export has no accounts array"))?;
                for account in accounts {
                    collect_import_auths(account, output)?;
                }
                return Ok(());
            }

            if let Some(credentials) = object.get("credentials").and_then(Value::as_object) {
                let platform = field(object, &["platform"]).unwrap_or("");
                let kind = field(object, &["type"]).unwrap_or("");
                if platform.eq_ignore_ascii_case("openai") || platform.eq_ignore_ascii_case("codex")
                {
                    if !kind.eq_ignore_ascii_case("oauth") {
                        return Ok(());
                    }
                    let mut credentials = credentials.clone();
                    if !credentials.contains_key("email")
                        && !credentials.contains_key("email_address")
                        && let Some(extra) = object.get("extra").and_then(Value::as_object)
                        && let Some(email) = field(extra, &["email_address", "email"])
                    {
                        credentials.insert("email".into(), Value::String(email.into()));
                    }
                    if let Some(name) = field(object, &["name"])
                        && name.contains('@')
                        && !credentials.contains_key("email")
                        && !credentials.contains_key("email_address")
                    {
                        credentials.insert("email".into(), Value::String(name.into()));
                    }
                    output.push(chatgpt_auth_from_export(&credentials, "Sub2API")?);
                }
                return Ok(());
            }

            if object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("codex"))
            {
                output.push(chatgpt_auth_from_export(object, "CLIProxyAPI")?);
                return Ok(());
            }

            if let Some(accounts) = object.get("accounts").and_then(Value::as_array) {
                for account in accounts {
                    collect_import_auths(account, output)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_import_auths(text: &str) -> Result<Vec<ParsedAuth>> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        Error::invalid(format!("The Codex credentials are not valid JSON: {error}"))
    })?;
    let mut accounts = Vec::new();
    collect_import_auths(&value, &mut accounts)?;
    if accounts.is_empty() {
        return Err(Error::invalid(
            "No Codex OAuth account was found. Import native Codex auth.json, a CLIProxyAPI codex file, or a Sub2API data export",
        ));
    }
    Ok(accounts)
}

fn read_auth(path: &Path) -> Result<Option<ParsedAuth>> {
    match fs::read_to_string(path) {
        Ok(text) => parse_auth(&text).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::path("read", path, error)),
    }
}

fn read_index(ctx: &Context) -> Result<AccountIndex> {
    let path = index_path(ctx);
    match fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => Ok(AccountIndex::default()),
        Ok(text) => serde_json::from_str(&text).map_err(|error| {
            Error::invalid(format!(
                "{} is not a valid account index: {error}",
                path.display()
            ))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AccountIndex::default()),
        Err(error) => Err(Error::path("read", &path, error)),
    }
}

fn secure_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fsx::ensure_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| Error::path("secure", path, error))
}

fn write_index(ctx: &Context, index: &AccountIndex) -> Result<()> {
    secure_dir(&accounts_root(ctx))?;
    let value = serde_json::to_value(index)?;
    fsx::atomic_write_json(&index_path(ctx), &value, Some(0o600))
}

fn save_credentials(ctx: &Context, id: &str, bytes: &[u8]) -> Result<()> {
    let dir = accounts_root(ctx).join("credentials");
    secure_dir(&dir)?;
    let mut text = bytes.to_vec();
    if !text.ends_with(b"\n") {
        text.push(b'\n');
    }
    fsx::atomic_write(&account_path(ctx, id), &text, Some(0o600))
}

fn load_credentials(ctx: &Context, account: &StoredAccount) -> Result<ParsedAuth> {
    let path = account_path(ctx, &account.id);
    let text = fs::read_to_string(&path)
        .map_err(|error| Error::path("read saved Codex credentials", &path, error))?;
    let parsed = parse_auth(&text)?;
    if parsed.fingerprint != account.fingerprint {
        return Err(Error::Conflict(
            "The saved Codex credential changed outside Cabin; import it again".into(),
        ));
    }
    Ok(parsed)
}

fn upsert(index: &mut AccountIndex, parsed: ParsedAuth, now: i64, used: bool) -> String {
    if let Some(account) = index
        .accounts
        .iter_mut()
        .find(|account| account.fingerprint == parsed.fingerprint)
    {
        account.email = parsed.email;
        account.plan_type = parsed.plan_type;
        account.auth_mode = parsed.auth_mode;
        account.subscription_until = parsed.subscription_until;
        if used {
            account.last_used_at = Some(now);
        }
        return account.id.clone();
    }
    let id = crate::util::new_id("codex");
    index.accounts.push(StoredAccount {
        id: id.clone(),
        fingerprint: parsed.fingerprint,
        email: parsed.email,
        plan_type: parsed.plan_type,
        auth_mode: parsed.auth_mode,
        subscription_until: parsed.subscription_until,
        imported_at: now,
        last_used_at: used.then_some(now),
        quota: None,
    });
    id
}

/// Keep the active snapshot current when Codex rotates refresh tokens.
fn sync_live(ctx: &Context) -> Result<Option<String>> {
    let Some(parsed) = read_auth(&auth_path(ctx))? else {
        let mut index = read_index(ctx)?;
        if index.active_account_id.take().is_some() {
            write_index(ctx, &index)?;
        }
        return Ok(None);
    };
    let mut index = read_index(ctx)?;
    let now = now_millis();
    let id = upsert(&mut index, parsed.clone(), now, false);
    save_credentials(ctx, &id, &parsed.bytes)?;
    index.active_account_id = Some(id.clone());
    write_index(ctx, &index)?;
    Ok(Some(id))
}

fn stored_account(account: &StoredAccount, active: bool) -> CodexAccount {
    CodexAccount {
        id: account.id.clone(),
        email: account.email.clone(),
        plan_type: account.plan_type.clone(),
        auth_mode: account.auth_mode.clone(),
        subscription_until: account.subscription_until.clone(),
        imported_at: account.imported_at,
        last_used_at: account.last_used_at,
        active,
    }
}

fn build_accounts(index: AccountIndex) -> CodexAccounts {
    let active_id = index.active_account_id.clone();
    let mut quota = active_id
        .as_deref()
        .and_then(|id| index.accounts.iter().find(|account| account.id == id))
        .and_then(|account| account.quota.clone());
    // Snapshots written before unit normalization may still hold seconds.
    if let Some(quota) = quota.as_mut() {
        normalize_quota(quota);
    }
    CodexAccounts {
        accounts: index
            .accounts
            .iter()
            .map(|account| stored_account(account, Some(&account.id) == active_id.as_ref()))
            .collect(),
        active_account_id: active_id,
        quota,
    }
}

fn normalize_quota(quota: &mut CodexQuota) {
    for window in &mut quota.windows {
        window.resets_at = window.resets_at.map(crate::util::normalize_unix_millis);
    }
    for card in &mut quota.reset_credits {
        card.granted_at = card.granted_at.map(crate::util::normalize_unix_millis);
        card.expires_at = card.expires_at.map(crate::util::normalize_unix_millis);
    }
}

pub(crate) fn accounts(ctx: &Context) -> Result<CodexAccounts> {
    sync_live(ctx)?;
    Ok(build_accounts(read_index(ctx)?))
}

/// Explicit export only: callers must treat the returned JSON as a live secret.
/// A native export accepts API keys; the interoperable OAuth formats require a
/// refresh token so an imported login can actually be renewed.
pub(crate) fn export_account(ctx: &Context, id: &str, format: &str) -> Result<String> {
    sync_live(ctx)?;
    let index = read_index(ctx)?;
    let account = index
        .accounts
        .iter()
        .find(|account| account.id == id)
        .ok_or_else(|| Error::NotFound("saved Codex account".into()))?;
    let parsed = load_credentials(ctx, account)?;
    let native: Value = serde_json::from_slice(&parsed.bytes)?;
    let value = match format {
        "native" => native,
        "cliproxyapi" | "sub2api" => {
            let tokens = native.get("tokens").and_then(Value::as_object)
                .ok_or_else(|| Error::invalid("This format requires a Codex ChatGPT OAuth login; export API keys as native auth.json"))?;
            let access = token_field(tokens, &["access_token"], "access_token")?;
            let refresh = token_field(tokens, &["refresh_token"], "refresh_token")?;
            let id_token = field(tokens, &["id_token"]).unwrap_or("");
            let account_id = field(tokens, &["account_id"]).unwrap_or("");
            let email = account.email.as_deref().unwrap_or("");
            let expires_at = jwt_payload(&access)
                .and_then(|claims| claims.get("exp").and_then(Value::as_i64))
                .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                .map(|date| date.to_rfc3339());
            if format == "cliproxyapi" {
                let mut export = json!({"type": "codex", "email": email, "plan_type": account.plan_type,
                    "id_token": id_token, "access_token": access, "refresh_token": refresh,
                    "account_id": account_id});
                if let Some(expires_at) = expires_at {
                    export["expired"] = expires_at.into();
                }
                export
            } else {
                let mut export = json!({"type": "sub2api-data", "version": 1, "proxies": [], "accounts": [{
                    "name": if email.is_empty() { "Codex account" } else { email },
                    "platform": "openai", "type": "oauth", "concurrency": 10, "priority": 1,
                    "rate_multiplier": 1, "auto_pause_on_expired": true,
                    "credentials": {"email": email, "plan_type": account.plan_type,
                        "id_token": id_token, "access_token": access, "refresh_token": refresh,
                        "client_id": "app_EMoamEEZ73f0CkXaXp7hrann",
                        "chatgpt_account_id": account_id}
                }]});
                if let Some(expires_at) = expires_at {
                    export["accounts"][0]["credentials"]["expires_at"] = expires_at.into();
                }
                export
            }
        }
        _ => return Err(Error::invalid("Unknown Codex export format")),
    };
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}

pub(crate) fn import_account(
    ctx: &Context,
    contents: Option<String>,
    activate: bool,
) -> Result<CodexAccounts> {
    sync_live(ctx)?;
    let raw = match contents {
        Some(contents) => contents,
        None => fs::read_to_string(auth_path(ctx)).map_err(|error| {
            Error::path("read current Codex credentials", &auth_path(ctx), error)
        })?,
    };
    let parsed_accounts = parse_import_auths(&raw)?;
    let mut index = read_index(ctx)?;
    let mut imported_ids = Vec::with_capacity(parsed_accounts.len());
    let mut previous_credentials: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    let write_result = (|| {
        for parsed in parsed_accounts {
            let id = upsert(&mut index, parsed.clone(), now_millis(), false);
            if !previous_credentials.iter().any(|(seen, _)| seen == &id) {
                let path = account_path(ctx, &id);
                let previous = match fs::read(&path) {
                    Ok(bytes) => Some(bytes),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => {
                        return Err(Error::path(
                            "read existing saved Codex credentials",
                            &path,
                            error,
                        ));
                    }
                };
                previous_credentials.push((id.clone(), previous));
            }
            save_credentials(ctx, &id, &parsed.bytes)?;
            imported_ids.push(id);
        }
        write_index(ctx, &index)
    })();
    if let Err(error) = write_result {
        if let Err(rollback) = rollback_import_credentials(ctx, &previous_credentials) {
            return Err(Error::Internal(format!(
                "Codex credential import failed ({error}); restoring the previous credentials also failed: {rollback}"
            )));
        }
        return Err(error);
    }
    if activate && let Some(id) = imported_ids.first() {
        switch_account(ctx, id)?;
    }
    accounts(ctx)
}

fn rollback_import_credentials(
    ctx: &Context,
    previous: &[(String, Option<Vec<u8>>)],
) -> Result<()> {
    for (id, bytes) in previous.iter().rev() {
        restore_credentials(&account_path(ctx, id), bytes.as_deref())?;
    }
    Ok(())
}

fn restore_credentials(path: &Path, previous: Option<&[u8]>) -> Result<()> {
    match previous {
        Some(bytes) => {
            let mut bytes = bytes.to_vec();
            if !bytes.ends_with(b"\n") {
                bytes.push(b'\n');
            }
            fsx::atomic_write(path, &bytes, Some(0o600))
        }
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Error::path("remove", path, error)),
        },
    }
}

pub(crate) fn switch_account(ctx: &Context, id: &str) -> Result<CodexAccounts> {
    sync_live(ctx)?;
    let mut index = read_index(ctx)?;
    let account = index
        .accounts
        .iter()
        .find(|account| account.id == id)
        .cloned()
        .ok_or_else(|| Error::NotFound("saved Codex account".into()))?;
    let parsed = load_credentials(ctx, &account)?;
    let path = auth_path(ctx);
    let previous = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(Error::path("read", &path, error)),
    };
    check_file_store(ctx)?;
    let backup_dir = ctx.paths.backups().join("agents").join("codex");
    let _ = fsx::backup(&path, &backup_dir)?;
    let mut bytes = parsed.bytes.clone();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    fsx::atomic_write(&path, &bytes, Some(0o600))?;
    let verified = read_auth(&path)?.is_some_and(|auth| auth.fingerprint == parsed.fingerprint);
    if !verified {
        restore_credentials(&path, previous.as_deref())?;
        return Err(Error::Internal(
            "Codex did not read the switched credentials back".into(),
        ));
    }
    index.active_account_id = Some(account.id.clone());
    if let Some(active) = index.accounts.iter_mut().find(|item| item.id == account.id) {
        active.last_used_at = Some(now_millis());
    }
    if let Err(error) = write_index(ctx, &index) {
        let _ = restore_credentials(&path, previous.as_deref());
        return Err(error);
    }
    accounts(ctx)
}

pub(crate) fn remove_account(ctx: &Context, id: &str) -> Result<CodexAccounts> {
    sync_live(ctx)?;
    let mut index = read_index(ctx)?;
    if index.active_account_id.as_deref() == Some(id) {
        return Err(Error::Conflict(
            "Switch to another Codex account before removing the active one".into(),
        ));
    }
    let position = index
        .accounts
        .iter()
        .position(|account| account.id == id)
        .ok_or_else(|| Error::NotFound("saved Codex account".into()))?;
    let original = index.clone();
    index.accounts.remove(position);
    write_index(ctx, &index)?;
    match fs::remove_file(account_path(ctx, id)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            write_index(ctx, &original).map_err(|rollback| {
                Error::Internal(format!(
                    "Could not remove saved Codex credentials ({error}); restoring the account index also failed: {rollback}"
                ))
            })?;
            return Err(Error::path(
                "remove saved Codex credentials",
                &account_path(ctx, id),
                error,
            ));
        }
    }
    Ok(build_accounts(index))
}

fn app_server_rpc(
    ctx: &Context,
    binary: &Path,
    requests: &[(&str, Value)],
) -> Result<Vec<std::result::Result<Value, String>>> {
    let mut child = Command::new(binary)
        .arg("app-server")
        .env("PATH", ctx.paths.child_path())
        .envs(environment::child_env(ctx))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| Error::io("start Codex app-server", error))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Internal("missing Codex app-server stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Internal("missing Codex app-server stdout".into()))?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(25);
    let send = |stdin: &mut std::process::ChildStdin, value: Value| -> Result<()> {
        writeln!(stdin, "{value}")
            .and_then(|()| stdin.flush())
            .map_err(|error| Error::io("write to Codex app-server", error))
    };
    let result = (|| {
        send(
            &mut stdin,
            json!({
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "cabin",
                        "title": "Cabin",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": { "experimentalApi": true }
                }
            }),
        )?;
        if let Err(message) = wait_rpc(&receiver, 1, deadline)? {
            return Err(Error::Unavailable(format!(
                "Codex app-server initialization failed: {message}"
            )));
        }
        send(&mut stdin, json!({"method": "initialized"}))?;
        let mut output = Vec::with_capacity(requests.len());
        for (offset, (method, params)) in requests.iter().enumerate() {
            let id = offset as u64 + 2;
            send(
                &mut stdin,
                json!({"id": id, "method": method, "params": params}),
            )?;
            output.push(wait_rpc(&receiver, id, deadline)?);
        }
        Ok(output)
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn wait_rpc(
    receiver: &mpsc::Receiver<std::io::Result<String>>,
    id: u64,
    deadline: Instant,
) -> Result<std::result::Result<Value, String>> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Unavailable("Codex app-server timed out".into()));
        }
        let line = receiver
            .recv_timeout(remaining)
            .map_err(|_| Error::Unavailable("Codex app-server timed out".into()))?
            .map_err(|error| Error::io("read from Codex app-server", error))?;
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if frame.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = frame.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Codex request failed");
            return Ok(Err(message.to_string()));
        }
        return Ok(Ok(frame.get("result").cloned().unwrap_or(Value::Null)));
    }
}

fn quota_window(value: Option<&Value>, position: &str) -> Option<CodexQuotaWindow> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    let used_percent = value
        .get("usedPercent")
        .or_else(|| value.get("used_percent"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(100) as u8;
    let window_minutes = value
        .get("windowDurationMins")
        .or_else(|| value.get("limit_window_seconds"))
        .and_then(Value::as_u64)
        .map(|duration| {
            if value.get("limit_window_seconds").is_some() {
                duration / 60
            } else {
                duration
            }
        });
    let resets_at = value
        .get("resetsAt")
        .or_else(|| value.get("reset_at"))
        .and_then(Value::as_i64)
        .map(crate::util::normalize_unix_millis);
    let (kind, label) = match window_minutes {
        Some(300) => ("fiveHour", "5-hour limit"),
        Some(10_080) => ("sevenDay", "7-day limit"),
        _ if position == "primary" => ("other", "Primary limit"),
        _ => ("other", "Secondary limit"),
    };
    Some(CodexQuotaWindow {
        kind: kind.into(),
        label: label.into(),
        used_percent,
        window_minutes,
        resets_at,
    })
}

fn quota_value(value: &Value, plan_fallback: Option<String>, error: Option<String>) -> CodexQuota {
    let rate = value.get("rateLimits").unwrap_or(&Value::Null);
    let mut windows = Vec::new();
    if let Some(window) = quota_window(rate.get("primary"), "primary") {
        windows.push(window);
    }
    if let Some(window) = quota_window(rate.get("secondary"), "secondary") {
        windows.push(window);
    }
    let credits = rate.get("credits").and_then(|credits| {
        if credits.is_null() {
            return None;
        }
        Some(CodexCredits {
            has_credits: credits
                .get("hasCredits")
                .or_else(|| credits.get("has_credits"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            unlimited: credits
                .get("unlimited")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            balance: safe_claim(credits.get("balance")),
        })
    });
    let reset_summary = value.get("rateLimitResetCredits");
    let reset_credit_count = reset_summary
        .and_then(|summary| {
            summary
                .get("availableCount")
                .or_else(|| summary.get("available_count"))
        })
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    let mut reset_credits: Vec<_> = reset_summary
        .and_then(|summary| summary.get("credits"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|credit| {
            let id = credit.get("id")?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            Some(CodexResetCard {
                id: id.to_string(),
                status: credit
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .into(),
                reset_type: credit
                    .get("resetType")
                    .or_else(|| credit.get("reset_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .into(),
                granted_at: credit
                    .get("grantedAt")
                    .or_else(|| credit.get("granted_at"))
                    .and_then(Value::as_i64)
                    .map(crate::util::normalize_unix_millis),
                expires_at: credit
                    .get("expiresAt")
                    .or_else(|| credit.get("expires_at"))
                    .and_then(Value::as_i64)
                    .map(crate::util::normalize_unix_millis),
                title: safe_claim(credit.get("title")),
                description: safe_claim(credit.get("description")),
            })
        })
        .collect();
    reset_credits.sort_by_key(|credit| credit.expires_at.unwrap_or(i64::MAX));
    CodexQuota {
        plan_type: rate
            .get("planType")
            .or_else(|| rate.get("plan_type"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(plan_fallback),
        ordinary_usage_allowed: value.get("ordinaryUsageAllowed").and_then(Value::as_bool),
        spend_control_reached: rate.get("spendControlReached").and_then(Value::as_bool),
        rate_limit_reached_type: rate
            .get("rateLimitReachedType")
            .and_then(Value::as_str)
            .map(str::to_string),
        windows,
        credits,
        reset_credit_count,
        reset_credits,
        refreshed_at: now_millis(),
        error,
    }
}

fn account_value(value: &Value) -> (Option<String>, Option<String>) {
    let account = value.get("account").unwrap_or(&Value::Null);
    (
        safe_claim(account.get("email")),
        safe_claim(account.get("planType")),
    )
}

fn update_quota(ctx: &Context, quota: CodexQuota, plan: Option<String>) -> Result<()> {
    let mut index = read_index(ctx)?;
    let Some(active_id) = index.active_account_id.clone() else {
        return Ok(());
    };
    if let Some(account) = index
        .accounts
        .iter_mut()
        .find(|account| account.id == active_id)
    {
        if plan.is_some() {
            account.plan_type = plan;
        }
        account.quota = Some(quota);
    }
    write_index(ctx, &index)
}

pub(crate) fn refresh_quota(ctx: &Context) -> Result<CodexAccounts> {
    sync_live(ctx)?;
    let binary = find_binary(ctx, AgentKind::Codex)
        .ok_or_else(|| Error::Unavailable("Codex is not installed".into()))?;
    let responses = app_server_rpc(
        ctx,
        &binary,
        &[
            ("account/read", json!({"refreshToken": false})),
            ("account/rateLimits/read", json!({})),
        ],
    )?;
    let (email, plan) = responses
        .first()
        .and_then(|response| response.as_ref().ok())
        .map(account_value)
        .unwrap_or((None, None));
    let quota = match responses.get(1) {
        Some(Ok(value)) => quota_value(value, plan.clone(), None),
        Some(Err(error)) => quota_value(&Value::Null, plan.clone(), Some(error.clone())),
        None => quota_value(
            &Value::Null,
            plan.clone(),
            Some("Codex returned no quota".into()),
        ),
    };
    if let Some(email) = email {
        let mut index = read_index(ctx)?;
        if let Some(active_id) = index.active_account_id.as_deref()
            && let Some(account) = index
                .accounts
                .iter_mut()
                .find(|account| account.id == active_id)
        {
            account.email = Some(email);
        }
        write_index(ctx, &index)?;
    }
    update_quota(ctx, quota, plan)?;
    // A usage read may refresh the access token and rotate the refresh token.
    sync_live(ctx)?;
    Ok(build_accounts(read_index(ctx)?))
}

pub(crate) fn use_reset_card(
    ctx: &Context,
    credit_id: &str,
    confirmed: bool,
) -> Result<CodexResetResult> {
    if !confirmed {
        return Err(Error::Conflict(
            "Using a Codex reset card is irreversible and requires confirmation".into(),
        ));
    }
    let credit_id = credit_id.trim();
    if credit_id.is_empty() || credit_id.len() > 512 || credit_id.chars().any(char::is_control) {
        return Err(Error::invalid("Choose a valid Codex reset card"));
    }
    let accounts = refresh_quota(ctx)?;
    let quota = accounts
        .quota
        .as_ref()
        .ok_or_else(|| Error::Unavailable("Codex did not return a quota reading".into()))?;
    if let Some(error) = &quota.error {
        return Err(Error::Unavailable(error.clone()));
    }
    if !quota
        .reset_credits
        .iter()
        .any(|credit| credit.id == credit_id && credit.status.eq_ignore_ascii_case("available"))
    {
        return Err(Error::Conflict(
            "That reset card is no longer available; refresh before trying again".into(),
        ));
    }
    let binary = find_binary(ctx, AgentKind::Codex)
        .ok_or_else(|| Error::Unavailable("Codex is not installed".into()))?;
    let responses = app_server_rpc(
        ctx,
        &binary,
        &[(
            "account/rateLimitResetCredit/consume",
            json!({
                "creditId": credit_id,
                "idempotencyKey": uuid::Uuid::new_v4().to_string()
            }),
        )],
    )?;
    let response = responses
        .into_iter()
        .next()
        .ok_or_else(|| Error::Unavailable("Codex returned no reset result".into()))?
        .map_err(Error::Unavailable)?;
    let outcome = response
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let message = match outcome.as_str() {
        "reset" => "The selected Codex limits were reset.",
        "nothingToReset" => "Nothing needs reset; no reset card was spent.",
        "noCredit" => "No reset card is available.",
        "alreadyRedeemed" => "That reset request was already completed.",
        _ => "Codex returned an unknown reset result.",
    }
    .to_string();
    let quota = refresh_quota(ctx)?.quota;
    Ok(CodexResetResult {
        outcome,
        message,
        quota,
    })
}

pub(super) fn auth_status(ctx: &Context, binary: &Path) -> (bool, String) {
    let output = ctx.system.run(
        &CommandSpec::new([
            binary.display().to_string(),
            "login".into(),
            "status".into(),
        ])
        .env([("PATH".into(), ctx.paths.child_path())])
        .timeout(Duration::from_secs(15)),
    );
    let parsed = read_auth(&auth_path(ctx)).ok().flatten();
    match output {
        Ok(output) if output.success() => {
            let detail = match parsed {
                Some(parsed) if parsed.auth_mode == "chatgpt" => {
                    let who = parsed.email.unwrap_or_else(|| "ChatGPT account".into());
                    match parsed.plan_type {
                        Some(plan) => format!("Signed in as {who} · {plan}"),
                        None => format!("Signed in as {who}"),
                    }
                }
                Some(parsed) if parsed.auth_mode == "apikey" => {
                    "Signed in with an OpenAI API key".into()
                }
                _ => output
                    .stdout
                    .lines()
                    .next()
                    .unwrap_or("Signed in")
                    .trim()
                    .to_string(),
            };
            (true, detail)
        }
        Ok(output) => {
            let detail = output.combined();
            let detail = detail.lines().next().unwrap_or("Not signed in").trim();
            (
                false,
                if detail.is_empty() {
                    "Not signed in. Run `codex login` in a session.".into()
                } else {
                    detail.to_string()
                },
            )
        }
        Err(error) => (false, format!("Could not check Codex login state: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::AppPaths;
    use crate::store::Store;
    use crate::system::fake::FakeSystem;
    use std::sync::Arc;

    fn token(email: &str, account: &str, plan: &str) -> String {
        let payload = json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account,
                "chatgpt_plan_type": plan,
                "chatgpt_subscription_active_until": "2099-01-01T00:00:00Z"
            }
        });
        format!(
            "{}.{}.sig",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{}"),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string())
        )
    }

    fn auth(email: &str, account: &str, plan: &str) -> String {
        let token = token(email, account, plan);
        json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": token,
                "id_token": token,
                "refresh_token": format!("refresh-{account}"),
                "account_id": account
            }
        })
        .to_string()
    }

    fn context(dir: &Path) -> Context {
        Context {
            paths: AppPaths::new(dir.join("data"), dir.join("home")),
            store: Arc::new(Store::in_memory().unwrap()),
            system: Arc::new(FakeSystem::default()),
        }
    }

    #[test]
    fn parses_identity_and_switches_saved_accounts() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let first = auth("one@example.com", "acct-one", "plus");
        fs::create_dir_all(codex_home(&ctx))?;
        fs::write(auth_path(&ctx), &first)?;
        let current = accounts(&ctx)?;
        assert_eq!(current.accounts.len(), 1);
        assert_eq!(
            current.accounts[0].email.as_deref(),
            Some("one@example.com")
        );
        assert_eq!(current.accounts[0].plan_type.as_deref(), Some("plus"));
        let last_used = current.accounts[0].last_used_at;
        std::thread::sleep(Duration::from_millis(2));
        let listed_again = accounts(&ctx)?;
        assert_eq!(listed_again.accounts[0].last_used_at, last_used);

        let imported =
            import_account(&ctx, Some(auth("two@example.com", "acct-two", "pro")), true)?;
        assert_eq!(imported.accounts.len(), 2);
        let active = imported
            .accounts
            .iter()
            .find(|account| account.active)
            .unwrap();
        assert_eq!(active.email.as_deref(), Some("two@example.com"));
        let live = read_auth(&auth_path(&ctx))?.unwrap();
        assert_eq!(live.email.as_deref(), Some("two@example.com"));
        let switched = import_account(
            &ctx,
            Some(auth("three@example.com", "acct-three", "team")),
            false,
        )?;
        let imported_three = switched
            .accounts
            .iter()
            .find(|account| account.email.as_deref() == Some("three@example.com"))
            .unwrap();
        assert_eq!(imported_three.last_used_at, None);

        let first_id = current.accounts[0].id.clone();
        let removed = remove_account(&ctx, &first_id)?;
        assert!(
            removed
                .accounts
                .iter()
                .all(|account| account.id != first_id)
        );
        assert!(!account_path(&ctx, &first_id).exists());
        Ok(())
    }

    #[test]
    fn imports_cliproxyapi_and_sub2api_exports() -> Result<()> {
        let cpa = json!({
            "type": "codex",
            "email": "cpa@example.com",
            "id_token": token("cpa@example.com", "acct-cpa", "plus"),
            "access_token": token("cpa@example.com", "acct-cpa", "plus"),
            "refresh_token": "refresh-cpa",
            "account_id": "acct-cpa",
            "expired": "2099-01-01T00:00:00Z"
        });
        let cpa_accounts = parse_import_auths(&cpa.to_string())?;
        assert_eq!(cpa_accounts.len(), 1);
        assert_eq!(cpa_accounts[0].email.as_deref(), Some("cpa@example.com"));
        assert_eq!(cpa_accounts[0].plan_type.as_deref(), Some("plus"));
        let normalized: Value = serde_json::from_slice(&cpa_accounts[0].bytes)?;
        assert_eq!(
            normalized["tokens"]["refresh_token"].as_str(),
            Some("refresh-cpa")
        );
        assert_eq!(
            normalized["tokens"]["account_id"].as_str(),
            Some("acct-cpa")
        );
        assert_eq!(normalized["auth_mode"].as_str(), Some("chatgpt"));

        let sub2api = json!({
            "type": "sub2api-data",
            "version": 1,
            "accounts": [
                {
                    "name": "team@example.com",
                    "platform": "openai",
                    "type": "oauth",
                    "credentials": {
                        "email": "team@example.com",
                        "id_token": token("team@example.com", "acct-seat-one", "team"),
                        "access_token": token("team@example.com", "acct-seat-one", "team"),
                        "refresh_token": "refresh-seat-one",
                        "chatgpt_account_id": "acct-seat-one"
                    }
                },
                {
                    "name": "Team seat two",
                    "platform": "openai",
                    "type": "oauth",
                    "extra": {"email_address": "team@example.com"},
                    "credentials": {
                        "id_token": token("team@example.com", "acct-seat-two", "team"),
                        "access_token": token("team@example.com", "acct-seat-two", "team"),
                        "refresh_token": "refresh-seat-two",
                        "chatgpt_account_id": "acct-seat-two"
                    }
                },
                {
                    "name": "user@example.com",
                    "platform": "vendor",
                    "type": "oauth",
                    "credentials": {
                        "access_token": "not-codex",
                        "refresh_token": "not-codex"
                    }
                },
                {
                    "name": "key",
                    "platform": "openai",
                    "type": "apikey",
                    "credentials": {"api_key": "sk-not-oauth"}
                }
            ]
        });
        let sub2api_accounts = parse_import_auths(&sub2api.to_string())?;
        assert_eq!(sub2api_accounts.len(), 2);
        assert_eq!(
            sub2api_accounts[0].email.as_deref(),
            Some("team@example.com")
        );
        assert_eq!(
            sub2api_accounts[1].email.as_deref(),
            Some("team@example.com")
        );
        assert_ne!(
            sub2api_accounts[0].fingerprint,
            sub2api_accounts[1].fingerprint
        );

        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let imported = import_account(&ctx, Some(cpa.to_string()), true)?;
        assert_eq!(imported.accounts.len(), 1);
        assert!(
            imported
                .accounts
                .iter()
                .all(|account| account.email.as_deref() == Some("cpa@example.com"))
        );
        let live = read_auth(&auth_path(&ctx))?.unwrap();
        let live_json: Value = serde_json::from_slice(&live.bytes)?;
        assert_eq!(
            live_json["tokens"]["refresh_token"].as_str(),
            Some("refresh-cpa")
        );
        let imported_sub2api = import_account(&ctx, Some(sub2api.to_string()), false)?;
        assert_eq!(imported_sub2api.accounts.len(), 3);
        assert_eq!(
            imported_sub2api
                .accounts
                .iter()
                .filter(|account| account.email.as_deref() == Some("team@example.com"))
                .count(),
            2
        );

        let access_only = json!({
            "type": "codex",
            "access_token": token("short@example.com", "acct-short", "free")
        });
        let error = parse_import_auths(&access_only.to_string())
            .err()
            .expect("an access-only account must be refused");
        assert!(error.to_string().contains("refresh_token"), "{error}");
        Ok(())
    }

    #[test]
    fn exports_round_trip_and_imports_identity_hints() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        // Some third-party files have identity only in the export, not the JWT.
        let access = format!(
            "e30.{}.sig",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"exp":1900000000}"#)
        );
        let source = json!({"type":"codex", "email":"hint@example.com", "plan_type":"plus",
            "access_token":access, "refresh_token":"opaque-refresh", "account_id":"seat-1"});
        let imported = import_account(&ctx, Some(source.to_string()), false)?;
        let id = &imported.accounts[0].id;
        for format in ["native", "cliproxyapi", "sub2api"] {
            let exported = export_account(&ctx, id, format)?;
            let parsed = parse_import_auths(&exported)?;
            assert_eq!(parsed.len(), 1);
            assert_eq!(parsed[0].email.as_deref(), Some("hint@example.com"));
            assert_eq!(
                parsed[0].fingerprint,
                read_index(&ctx)?.accounts[0].fingerprint
            );
            let decoded: Value = serde_json::from_str(&exported)?;
            if format == "cliproxyapi" {
                assert_eq!(decoded["expired"], "2030-03-17T17:46:40+00:00");
            } else if format == "sub2api" {
                assert_eq!(
                    decoded["accounts"][0]["credentials"]["client_id"],
                    "app_EMoamEEZ73f0CkXaXp7hrann"
                );
                assert_eq!(
                    decoded["accounts"][0]["credentials"]["expires_at"],
                    "2030-03-17T17:46:40+00:00"
                );
            }
            assert!(!decoded.to_string().contains("cabin codex account"));
        }
        let switched = switch_account(&ctx, id)?;
        assert_eq!(switched.active_account_id.as_deref(), Some(id.as_str()));
        assert!(export_account(&ctx, id, "unexpected").is_err());
        assert!(export_account(&ctx, "missing", "native").is_err());
        Ok(())
    }

    #[test]
    fn export_api_key_uses_native_format() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let imported = import_account(
            &ctx,
            Some(json!({"OPENAI_API_KEY": "sk-test-only"}).to_string()),
            false,
        )?;
        let id = &imported.accounts[0].id;
        let native = export_account(&ctx, id, "native")?;
        assert_eq!(parse_import_auths(&native)?.len(), 1);
        assert!(export_account(&ctx, id, "cliproxyapi").is_err());
        Ok(())
    }

    #[test]
    fn maps_five_hour_week_credits_and_reset_cards() {
        let value = json!({
            "ordinaryUsageAllowed": true,
            "rateLimits": {
                "planType": "pro",
                "primary": {
                    "usedPercent": 49,
                    "windowDurationMins": 300,
                    "resetsAt": 1900000000
                },
                "secondary": {
                    "usedPercent": 73,
                    "windowDurationMins": 10080,
                    "resetsAt": 1900500000
                },
                "credits": {
                    "hasCredits": true,
                    "unlimited": false,
                    "balance": "62500"
                }
            },
            "rateLimitResetCredits": {
                "availableCount": 1,
                "credits": [{
                    "id": "reset-a",
                    "resetType": "codexRateLimits",
                    "status": "available",
                    "grantedAt": 1899000000,
                    "expiresAt": 1901000000
                }]
            }
        });
        let quota = quota_value(&value, None, None);
        assert_eq!(quota.plan_type.as_deref(), Some("pro"));
        assert_eq!(quota.windows[0].kind, "fiveHour");
        assert_eq!(quota.windows[0].resets_at, Some(1_900_000_000_000));
        assert_eq!(quota.windows[1].kind, "sevenDay");
        assert_eq!(quota.windows[1].resets_at, Some(1_900_500_000_000));
        assert_eq!(quota.reset_credit_count, 1);
        assert_eq!(quota.reset_credits[0].id, "reset-a");
        assert_eq!(quota.reset_credits[0].granted_at, Some(1_899_000_000_000));
        assert_eq!(quota.reset_credits[0].expires_at, Some(1_901_000_000_000));

        // Snapshots saved before this normalization still carry seconds.
        let mut legacy = quota.clone();
        legacy.windows[0].resets_at = Some(1_900_000_000);
        normalize_quota(&mut legacy);
        assert_eq!(legacy.windows[0].resets_at, Some(1_900_000_000_000));
    }
}
