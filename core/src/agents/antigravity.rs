use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use serde_json::Value;

use super::extensions::{McpCodec, json};
use super::{AgentAdapter, AgentSpec, LaunchOptions};
use crate::context::Context;
use crate::error::{Error, Result};
use crate::model::{AgentKind, McpServer, McpTransport};
use crate::ops::{Operation, Step, StepCtx};
use crate::system::CommandSpec;

pub struct Antigravity;

/// The app's model picker leaves the native model selection in effect.
pub const DEFAULT_MODEL: &str = "default";
const VERSION: &str = "1.5.14";
const ARCHIVE_URL: &str = "https://github.com/google-antigravity/antigravity-cli/releases/download/1.5.14/agy_cli_linux_arm64.tar.gz";
const ARCHIVE_SHA256: &str = "e5f69a3123fac66974b303d9a201f381a0fb32b3ef869d9139da85d5caf67a2a";
const OAUTH_FILE: &str = ".gemini/antigravity-cli/antigravity-oauth-token";

static SPEC: AgentSpec = AgentSpec {
    kind: AgentKind::Antigravity,
    name: "Antigravity",
    binary: "agy",
    package: "agy",
    config: ".gemini/antigravity-cli/settings.json",
    model_key: "model",
    auth_files: &[OAUTH_FILE],
    auth_env: &[],
};

impl AgentAdapter for Antigravity {
    fn spec(&self) -> &'static AgentSpec {
        &SPEC
    }

    fn requires_node(&self) -> bool {
        false
    }

    fn launch_env(&self) -> Vec<(String, String)> {
        vec![("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into())]
    }

    fn launch_args(&self, options: &LaunchOptions) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(model) = options
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty() && *model != DEFAULT_MODEL)
        {
            args.push(format!("--model={model}"));
        }
        if options.resume {
            args.push("--continue".into());
        } else if let Some(prompt) = options
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
        {
            args.extend(self.prompt_args(prompt));
        }
        args
    }

    fn prompt_args(&self, prompt: &str) -> Vec<String> {
        vec![format!("--prompt-interactive={prompt}")]
    }

    fn mcp(&self) -> Option<&'static dyn McpCodec> {
        Some(&Mcp)
    }

    fn skills_dir(&self, ctx: &Context) -> PathBuf {
        ctx.paths.home().join(".gemini/config/skills")
    }

    fn project_skills_dir(&self) -> &'static str {
        ".agents/skills"
    }

    fn skill_dirs(&self, ctx: &Context, workspace: Option<&Path>) -> Vec<(PathBuf, &'static str)> {
        let home = ctx.paths.home();
        let mut dirs = vec![
            (home.join(".gemini/antigravity-cli/skills"), "user"),
            (home.join(".gemini/skills"), "user"),
        ];
        if let Some(root) = workspace {
            dirs.push((root.join(self.project_skills_dir()), "project"));
        }
        dirs.push((self.skills_dir(ctx), "user"));
        dirs
    }
}

/// Inspect only the metadata of the native file used for SSH sign-in.
/// Antigravity can also store credentials in the system keyring.
pub fn auth(ctx: &Context, binary: Option<&Path>) -> (bool, String) {
    if binary.is_none() {
        return (false, "Install Antigravity to sign in.".into());
    }
    if fs::metadata(ctx.paths.home().join(OAUTH_FILE))
        .is_ok_and(|meta| meta.is_file() && meta.len() > 0)
    {
        return (
            true,
            "Cached Google sign-in found. Antigravity checks it when it connects.".into(),
        );
    }
    (
        false,
        "No cached Google sign-in file found. Antigravity checks keyring and API-key sign-in when it connects."
            .into(),
    )
}

/// The native global MCP config. Antigravity discovers `.agents/mcp_config.json`
/// itself in each workspace.
struct Mcp;

const MCP_KEY: &str = "mcpServers";

impl McpCodec for Mcp {
    fn path(&self, ctx: &Context) -> PathBuf {
        ctx.paths.home().join(".gemini/config/mcp_config.json")
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
        let mut root = json::parse(Some(text))?;
        Ok(json::table(&mut root, MCP_KEY)?
            .iter()
            .filter_map(|(name, entry)| {
                let transport = match entry.get("serverUrl").or_else(|| entry.get("url")) {
                    Some(url) => McpTransport::Http {
                        url: url.as_str()?.into(),
                        headers: json::strings(entry.get("headers"))?,
                        sse: false,
                    },
                    None => McpTransport::Stdio {
                        command: entry.get("command")?.as_str()?.into(),
                        args: json::list(entry.get("args"))?,
                        env: json::strings(entry.get("env"))?,
                    },
                };
                let disabled = entry.get("disabled").map_or(Some(false), Value::as_bool)?;
                Some(McpServer {
                    name: name.clone(),
                    transport,
                    enabled: !disabled,
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
        let old_name = previous.unwrap_or(&server.name);
        if servers
            .get(old_name)
            .is_some_and(|entry| !entry.is_object())
        {
            return Err(Error::invalid(format!(
                "The Antigravity MCP entry for {old_name} is not an object"
            )));
        }
        let mut entry = json::take_entry(servers, &server.name, previous)?;
        match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                for key in ["serverUrl", "url", "headers"] {
                    entry.shift_remove(key);
                }
                entry.insert("command".into(), command.as_str().into());
                entry.insert(
                    "args".into(),
                    args.iter().map(|arg| Value::from(arg.as_str())).collect(),
                );
                if env.is_empty() {
                    entry.shift_remove("env");
                } else {
                    entry.insert("env".into(), json::object(env));
                }
            }
            McpTransport::Http { url, headers, sse } => {
                if *sse {
                    return Err(Error::invalid(
                        "Antigravity uses Streamable HTTP for remote MCP servers. Choose HTTP for a Streamable HTTP endpoint.",
                    ));
                }
                for key in ["command", "args", "env"] {
                    entry.shift_remove(key);
                }
                if entry.contains_key("serverUrl") || !entry.contains_key("url") {
                    entry.insert("serverUrl".into(), url.as_str().into());
                }
                if entry.contains_key("url") {
                    entry.insert("url".into(), url.as_str().into());
                }
                if headers.is_empty() {
                    entry.shift_remove("headers");
                } else {
                    entry.insert("headers".into(), json::object(headers));
                }
            }
        }
        if !server.enabled || entry.contains_key("disabled") {
            entry.insert("disabled".into(), (!server.enabled).into());
        }
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

pub fn install_operation(ctx: &Context) -> Operation {
    Operation::new("setup.installAgent", "Install Antigravity")
        .key("setup-agent-antigravity")
        .request(serde_json::json!({ "agent": AgentKind::Antigravity }))
        .step(Install {
            ctx: ctx.clone(),
            target: ctx.paths.home().join(".local/bin").join(SPEC.binary),
            staging: None,
            had_previous: false,
        })
}

struct Install {
    ctx: Context,
    target: PathBuf,
    /// Set after activation. A sibling staging directory makes renames atomic.
    staging: Option<tempfile::TempDir>,
    had_previous: bool,
}

impl Step for Install {
    fn name(&self) -> String {
        format!("Install agy {VERSION} for Linux ARM64")
    }

    fn prepare(&mut self, _: &StepCtx<'_>) -> Result<()> {
        // The daemon is built with musl and launches the CLI against Ubuntu's glibc.
        if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            return Err(Error::Unavailable(
                "The managed Antigravity build requires ARM64 Ubuntu.".into(),
            ));
        }
        Ok(())
    }

    fn apply(&mut self, step: &StepCtx<'_>) -> Result<()> {
        let bin = self.target.parent().expect("absolute path to agy");
        crate::fsx::ensure_dir(bin)?;
        let staging = tempfile::Builder::new()
            .prefix(".agy-install-")
            .tempdir_in(bin)?;
        let archive = staging.path().join("agy_cli_linux_arm64.tar.gz");
        let bytes = crate::fetch::download_to(&self.ctx, ARCHIVE_URL, &archive, step)?;
        step.log(format!("Downloaded {:.1} MB", bytes as f64 / 1e6));
        if crate::util::sha256_file(&archive)? != ARCHIVE_SHA256 {
            return Err(Error::invalid(
                "The Antigravity archive failed SHA-256 verification",
            ));
        }
        step.log("Verified the published SHA-256 checksum");
        if step.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let candidate = staging.path().join("agy");
        extract_executable(&archive, &candidate)?;
        if step.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.had_previous = preserve_previous(&self.target, &staging.path().join("previous"))?;
        fs::rename(&candidate, &self.target)
            .map_err(|error| Error::path("activate", &self.target, error))?;
        self.staging = Some(staging);
        Ok(())
    }

    fn verify(&mut self, step: &StepCtx<'_>) -> Result<()> {
        if step.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let output = self.ctx.system.check(
            &CommandSpec::new([self.target.display().to_string(), "--version".into()])
                .env(Antigravity.launch_env())
                .timeout(Duration::from_secs(15)),
        )?;
        if output.trim() != VERSION {
            return Err(Error::Internal(format!(
                "The installed agy executable did not report version {VERSION}"
            )));
        }
        if step.is_cancelled() {
            return Err(Error::Cancelled);
        }
        step.log(format!("{} → {VERSION}", self.target.display()));
        Ok(())
    }

    fn rollback(&mut self, step: &StepCtx<'_>) -> Result<()> {
        let Some(staging) = self.staging.take() else {
            return Ok(());
        };
        if self.had_previous {
            let previous = staging.path().join("previous");
            if let Err(error) = fs::rename(&previous, &self.target) {
                let retained = staging.keep().join("previous");
                return Err(Error::path("restore previous agy from", &retained, error));
            }
            step.log("Restored the previous agy executable");
        } else {
            if let Err(error) = fs::remove_file(&self.target)
                && error.kind() != io::ErrorKind::NotFound
            {
                return Err(Error::path("remove", &self.target, error));
            }
            step.log("Removed the incomplete Antigravity installation");
        }
        Ok(())
    }
}

fn extract_executable(archive: &Path, target: &Path) -> Result<()> {
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(archive)?));
    let mut entries = tar.entries()?;
    let mut binary = entries
        .next()
        .transpose()?
        .ok_or_else(|| Error::invalid("The Antigravity archive is empty"))?;
    if binary.path()?.as_ref() != Path::new("antigravity")
        || !binary.header().entry_type().is_file()
    {
        return Err(Error::invalid(
            "Expected an Antigravity executable in the release archive",
        ));
    }
    let mut file = File::create_new(target)?;
    io::copy(&mut binary, &mut file)?;
    if entries.next().is_some() {
        return Err(Error::invalid(
            "Unexpected extra entries in the Antigravity archive",
        ));
    }
    file.set_permissions(fs::Permissions::from_mode(0o755))?;
    file.sync_all()?;
    Ok(())
}

/// Leave the old executable in place until the atomic replacement succeeds.
fn preserve_previous(target: &Path, backup: &Path) -> Result<bool> {
    match fs::symlink_metadata(target) {
        Ok(meta) if meta.is_symlink() => symlink(fs::read_link(target)?, backup)?,
        Ok(meta) if meta.is_file() => fs::hard_link(target, backup)?,
        Ok(_) => {
            return Err(Error::invalid(format!(
                "{} is not an executable file",
                target.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(Error::path("inspect", target, error)),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use serde_json::json;

    #[test]
    fn terminal_uses_the_native_default_and_keeps_a_flag_like_prompt() {
        assert_eq!(
            Antigravity.launch_args(&LaunchOptions {
                model: Some(DEFAULT_MODEL.into()),
                resume: false,
                prompt: Some("--print this example".into()),
            }),
            ["--prompt-interactive=--print this example"]
        );
        assert_eq!(
            Antigravity.launch_args(&LaunchOptions {
                model: Some("Gemini 3.5 Flash".into()),
                resume: true,
                prompt: Some("ignored".into()),
            }),
            ["--model=Gemini 3.5 Flash", "--continue"]
        );
    }

    #[test]
    fn mcp_edits_keep_the_native_url_alias_and_unrelated_fields() -> Result<()> {
        let original = json!({
            "native": { "key": true },
            "mcpServers": {
                "remote": {
                    "url": "https://example.com/mcp",
                    "headers": { "X-MCP-Client": "agy" },
                    "disabled": true,
                    "disabledTools": ["diagnose"],
                },
                "local": {
                    "command": "example-mcp",
                    "args": ["--stdio"],
                    "env": { "MODE": "stdio" },
                },
            },
        });
        let text = original.to_string();
        let servers = Mcp.read(&text)?;
        let mut remote = servers
            .iter()
            .find(|server| server.name == "remote")
            .unwrap()
            .clone();
        assert!(!remote.enabled);
        remote.enabled = true;
        remote.name = "renamed".into();
        let updated: Value =
            serde_json::from_str(&Mcp.upsert(Some(&text), &remote, Some("remote"))?)?;
        assert_eq!(updated["native"], original["native"]);
        assert_eq!(
            updated["mcpServers"]["local"],
            original["mcpServers"]["local"]
        );
        let mut expected = original["mcpServers"]["remote"].clone();
        expected["disabled"] = Value::Bool(false);
        assert_eq!(updated["mcpServers"]["renamed"], expected);
        assert!(updated["mcpServers"].get("remote").is_none());

        let McpTransport::Http { ref mut sse, .. } = remote.transport else {
            panic!("remote should use Streamable HTTP");
        };
        *sse = true;
        assert!(Mcp.upsert(Some(&text), &remote, None).is_err());
        Ok(())
    }

    #[test]
    fn extraction_writes_only_the_expected_regular_file() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join("release.tar.gz");
        let target = dir.path().join("agy");
        let payload = b"mock executable";
        let mut tar =
            tar::Builder::new(GzEncoder::new(File::create(&archive)?, Compression::fast()));
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        tar.append_data(&mut header, "antigravity", payload.as_slice())?;
        tar.into_inner()?.finish()?;
        extract_executable(&archive, &target)?;
        assert_eq!(fs::read(&target)?, payload);
        assert_eq!(fs::metadata(&target)?.permissions().mode() & 0o777, 0o755);

        fs::remove_file(&target)?;
        let mut tar =
            tar::Builder::new(GzEncoder::new(File::create(&archive)?, Compression::fast()));
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        tar.append_link(&mut header, "antigravity", "../../other")?;
        tar.into_inner()?.finish()?;
        assert!(extract_executable(&archive, &target).is_err());
        assert!(!target.exists());
        Ok(())
    }
}
