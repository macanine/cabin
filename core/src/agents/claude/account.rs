use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, de::DeserializeOwned};

use crate::context::Context;
use crate::model::{ClaudeAccount, ClaudeAuthMethod, ClaudeLogin};
use crate::system::CommandSpec;
use crate::util::{now_millis, sha256_text};

const SIGN_IN: &str = "Not signed in. Run `claude auth login` in a machine terminal.";
const UNAVAILABLE: &str = "Claude Code's sign-in status is temporarily unavailable.";

pub(super) struct AccountFiles {
    credentials: PathBuf,
    profile: PathBuf,
    config_dir: Option<PathBuf>,
}

impl AccountFiles {
    pub(super) fn new(home: &Path) -> Self {
        Self::at(
            home,
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .as_deref(),
        )
    }

    pub(super) fn at(home: &Path, custom: Option<&Path>) -> Self {
        let config_dir = custom.map(|path| home.join(path));
        Self {
            credentials: config_dir
                .clone()
                .unwrap_or_else(|| home.join(".claude"))
                .join(".credentials.json"),
            profile: config_dir.as_deref().unwrap_or(home).join(".claude.json"),
            config_dir,
        }
    }

    pub(super) fn apply(&self, command: &mut CommandSpec) {
        if let Some(dir) = &self.config_dir {
            command
                .env
                .push(("CLAUDE_CONFIG_DIR".into(), dir.display().to_string()));
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct NativeStatus {
    logged_in: Option<bool>,
    auth_method: Option<String>,
    api_provider: Option<String>,
    email: Option<String>,
    org_name: Option<String>,
    #[serde(alias = "organizationUuid", alias = "organizationId")]
    org_id: Option<String>,
    subscription_type: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ProfileFile {
    oauth_account: Option<Profile>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Profile {
    email_address: Option<String>,
    account_uuid: Option<String>,
    organization_uuid: Option<String>,
    organization_name: Option<String>,
    plan_display_name: Option<String>,
    seat_tier: Option<String>,
    billing_type: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CredentialsFile {
    claude_ai_oauth: Option<OAuth>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct OAuth {
    access_token: Option<String>,
    expires_at: Option<i64>,
    rate_limit_tier: Option<String>,
    subscription_type: Option<String>,
}

fn read_json<T: DeserializeOwned>(path: &Path, max_bytes: u64) -> io::Result<Option<T>> {
    // A named pipe in place of a replaced config file must not block a probe.
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let before = file.metadata()?;
    if !before.is_file() || before.len() > max_bytes {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = Vec::new();
    file.by_ref().take(max_bytes + 1).read_to_end(&mut bytes)?;
    let after = path.metadata()?;
    if bytes.len() as u64 > max_bytes
        || before.len() != after.len()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    // Parse errors can contain file contents. The boundary only needs the kind.
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| io::ErrorKind::InvalidData.into())
}

fn label(value: Option<&str>, max_chars: usize) -> Option<&str> {
    value.map(str::trim).filter(|value| {
        !value.is_empty()
            && value.chars().count() <= max_chars
            && !value.chars().any(char::is_control)
    })
}

fn email(value: Option<&str>) -> Option<&str> {
    label(value, 320).filter(|value| value.contains('@') && !value.chars().any(char::is_whitespace))
}

fn disagree(a: Option<&str>, b: Option<&str>) -> bool {
    a.zip(b).is_some_and(|(a, b)| !a.eq_ignore_ascii_case(b))
}

fn plan_name(
    subscription: Option<&str>,
    tier: Option<&str>,
    shown: Option<&str>,
) -> Option<&'static str> {
    match subscription.or(shown)?.trim().to_ascii_lowercase().as_str() {
        "free" => Some("Free"),
        "pro" => Some("Pro"),
        "max" => match tier.unwrap_or_default() {
            tier if tier.contains("max_20x") => Some("Ultra"),
            tier if tier.contains("max_5x") => Some("Max"),
            _ if shown.is_some_and(|shown| shown.eq_ignore_ascii_case("ultra")) => Some("Ultra"),
            _ => Some("Max"),
        },
        "ultra" => Some("Ultra"),
        "team" => Some("Team"),
        "enterprise" => Some("Enterprise"),
        _ => None,
    }
}

fn native_provider(status: &NativeStatus) -> Option<&'static str> {
    match status.api_provider.as_deref() {
        None | Some("" | "firstParty" | "anthropic") => None,
        Some("bedrock") => Some("AWS Bedrock"),
        Some("vertex") => Some("Google Vertex AI"),
        Some("foundry") => Some("Microsoft Foundry"),
        Some(_) => Some("External provider"),
    }
}

fn env_method() -> Option<ClaudeAuthMethod> {
    let set = |name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty());
    if set("CLAUDE_CODE_OAUTH_TOKEN") {
        Some(ClaudeAuthMethod::OauthToken)
    } else if set("ANTHROPIC_API_KEY") || set("ANTHROPIC_AUTH_TOKEN") {
        Some(ClaudeAuthMethod::ApiKey)
    } else {
        None
    }
}

fn make_account(
    status: NativeStatus,
    profile: Option<Profile>,
    oauth: Option<OAuth>,
    environment: Option<ClaudeAuthMethod>,
) -> (Option<ClaudeAccount>, &'static str) {
    use ClaudeAuthMethod::{ApiKey, External, OauthToken, Subscription, Unknown};
    let provider = native_provider(&status);
    let verified = match status.logged_in {
        Some(false) => return (None, SIGN_IN),
        Some(true) => true,
        None => false,
    };
    let method = if verified {
        if provider.is_some() {
            External
        } else {
            match status.auth_method.as_deref() {
                Some("claude.ai") => Subscription,
                Some("oauth_token") => OauthToken,
                Some("api_key" | "apiKey" | "api_key_helper" | "console") => ApiKey,
                None if status.subscription_type.is_some() => Subscription,
                None => environment.unwrap_or(if oauth.is_some() {
                    Subscription
                } else {
                    Unknown
                }),
                _ => Unknown,
            }
        }
    } else if let Some(method) = environment {
        method
    } else if oauth.is_some() {
        Subscription
    } else {
        return (None, SIGN_IN);
    };

    let native_email = email(status.email.as_deref());
    let native_org = label(status.org_name.as_deref(), 256);
    let native_org_id = label(status.org_id.as_deref(), 128);
    let (email, organization, plan, usage_based, id) = if method == Subscription {
        let profile = profile.unwrap_or_default();
        let saved_email = email(profile.email_address.as_deref());
        let saved_org = label(profile.organization_name.as_deref(), 256);
        let saved_org_id = label(profile.organization_uuid.as_deref(), 128);
        // During a switch the CLI can already name the new seat while the
        // profile still describes the previous one.
        let reported_plan = label(status.subscription_type.as_deref(), 64);
        let saved_plan = oauth
            .as_ref()
            .and_then(|oauth| label(oauth.subscription_type.as_deref(), 64));
        let same_org = match (native_org_id, saved_org_id) {
            (Some(native), Some(saved)) => native.eq_ignore_ascii_case(saved),
            _ => !disagree(native_org, saved_org),
        };
        let matching_profile = !disagree(native_email, saved_email)
            && same_org
            && !disagree(reported_plan, saved_plan);
        let saved = matching_profile.then_some(&profile);
        let metadata = if matching_profile {
            oauth.as_ref()
        } else {
            None
        };
        let email = native_email.or(saved.and(saved_email));
        let organization = native_org.or(saved.and(saved_org));
        let organization_id = native_org_id.or(saved.and(saved_org_id));
        let account_id = saved.and_then(|profile| label(profile.account_uuid.as_deref(), 128));
        let plan = plan_name(
            label(status.subscription_type.as_deref(), 64)
                .or(metadata.and_then(|oauth| label(oauth.subscription_type.as_deref(), 64))),
            metadata.and_then(|oauth| label(oauth.rate_limit_tier.as_deref(), 64)),
            saved.and_then(|profile| label(profile.plan_display_name.as_deref(), 64)),
        );
        let usage_based = saved.is_some_and(|profile| {
            profile.seat_tier.as_deref() == Some("enterprise_usage_based")
                || profile.billing_type.as_deref() == Some("usage_based")
        });
        let principal = email.or(account_id).map(str::to_ascii_lowercase);
        let seat = organization_id
            .or(organization)
            .map(str::to_ascii_lowercase);
        // A plan alone cannot identify a seat. Keep the display metadata but
        // only scope a quota to an account when Claude gave it an identity.
        let id = principal.map(|principal| {
            sha256_text(&serde_json::json!([principal, seat, plan]).to_string())
        });
        (
            email.map(str::to_string),
            organization.map(str::to_string),
            plan.map(str::to_string),
            usage_based,
            id,
        )
    } else {
        (
            native_email.map(str::to_string),
            native_org.map(str::to_string),
            None,
            false,
            None,
        )
    };
    let detail = match (method, verified) {
        (Subscription, true) => "Signed in through Claude Code",
        (Subscription, false)
            if oauth
                .and_then(|oauth| oauth.expires_at)
                .is_some_and(|expiry| expiry > 0 && expiry <= now_millis()) =>
        {
            "Saved sign-in · token expired"
        }
        (Subscription, false) => "Saved sign-in",
        (ApiKey, true) => "API billing",
        (ApiKey, false) => "API credentials configured",
        (OauthToken, true) => "Signed in with an OAuth token",
        (OauthToken, false) => "OAuth token configured",
        (External, _) => provider.unwrap_or("External provider"),
        (Unknown, _) => "Signed in through Claude Code",
    };
    (
        Some(ClaudeAccount {
            id,
            method,
            verified,
            email,
            organization,
            plan,
            provider: provider.map(str::to_string),
            usage_based,
        }),
        detail,
    )
}

/// Read the current guest account using Claude Code's non-secret status view.
/// The profile and credential metadata complement it and cover older CLIs.
pub(in crate::agents) fn inspect(
    ctx: &Context,
    binary: Option<&Path>,
) -> (ClaudeLogin, &'static str) {
    inspect_at(ctx, binary, &AccountFiles::new(ctx.paths.home()))
}

pub(super) fn inspect_at(
    ctx: &Context,
    binary: Option<&Path>,
    files: &AccountFiles,
) -> (ClaudeLogin, &'static str) {
    let status = binary
        .and_then(|binary| {
            let mut command = CommandSpec::new([
                binary.display().to_string(),
                "auth".into(),
                "status".into(),
                "--json".into(),
            ])
            .env([
                ("DISABLE_AUTOUPDATER".into(), "1".into()),
                ("DISABLE_TELEMETRY".into(), "1".into()),
                ("DISABLE_ERROR_REPORTING".into(), "1".into()),
            ])
            .timeout(Duration::from_secs(5))
            .output_limit(64 * 1024);
            command.cwd = Some(ctx.paths.home().display().to_string());
            files.apply(&mut command);
            // Signed-out status is valid JSON even though Claude exits with 1.
            let output = ctx.system.run(&command).ok()?;
            serde_json::from_str::<NativeStatus>(&output.stdout).ok()
        })
        .unwrap_or_default();
    if status.logged_in == Some(false) {
        return (ClaudeLogin::SignedOut, SIGN_IN);
    }
    let profile = read_json::<ProfileFile>(&files.profile, 32 * 1024 * 1024)
        .ok()
        .flatten()
        .and_then(|file| file.oauth_account);
    let credentials = read_json::<CredentialsFile>(&files.credentials, 1024 * 1024);
    let unavailable = binary.is_some() || credentials.is_err();
    let oauth = credentials
        .ok()
        .flatten()
        .and_then(|file| file.claude_ai_oauth)
        .filter(|oauth| {
            oauth
                .access_token
                .as_deref()
                .is_some_and(|token| !token.trim().is_empty())
        });
    match make_account(status, profile, oauth, env_method()) {
        (Some(account), detail) => (ClaudeLogin::Active { account }, detail),
        (None, _) if unavailable => (ClaudeLogin::Unavailable, UNAVAILABLE),
        (None, _) => (ClaudeLogin::SignedOut, SIGN_IN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_seat_wins_over_an_outdated_profile_for_the_same_email() {
        let old = Profile {
            email_address: Some("dev@example.com".into()),
            organization_uuid: Some("personal-org".into()),
            organization_name: Some("Personal".into()),
            ..Profile::default()
        };
        let personal = make_account(
            NativeStatus {
                logged_in: Some(true),
                auth_method: Some("claude.ai".into()),
                ..NativeStatus::default()
            },
            Some(old),
            None,
            None,
        )
        .0
        .unwrap();
        let old: Profile = serde_json::from_str(r#"{"emailAddress":"dev@example.com","organizationUuid":"personal-org","organizationName":"Personal","planDisplayName":"Ultra"}"#).unwrap();
        let status: NativeStatus = serde_json::from_str(r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"dev@example.com","orgId":"team-org","orgName":"Example Studio","subscriptionType":"team"}"#).unwrap();
        let current = make_account(status, Some(old), None, None).0.unwrap();
        assert!(current.verified);
        assert_eq!(current.email, personal.email);
        assert_ne!(current.id, personal.id);
        assert_eq!(current.organization.as_deref(), Some("Example Studio"));
        assert_eq!(current.plan.as_deref(), Some("Team"));
    }

    #[test]
    fn saved_metadata_supplies_the_plan_without_exporting_credentials() {
        let config = tempfile::tempdir().unwrap();
        let files = AccountFiles::at(config.path(), Some(Path::new("claude-work")));
        assert_eq!(
            files.profile,
            config.path().join("claude-work/.claude.json")
        );
        assert_eq!(
            files.credentials,
            config.path().join("claude-work/.credentials.json")
        );
        std::fs::create_dir_all(files.profile.parent().unwrap()).unwrap();
        std::fs::write(&files.credentials, r#"{"claudeAiOauth":{"accessToken":"dummy-access-token","refreshToken":"dummy-refresh-token","subscriptionType":"max","rateLimitTier":"default_claude_max_20x"}}"#).unwrap();
        let oauth = read_json::<CredentialsFile>(&files.credentials, 1024)
            .unwrap()
            .unwrap()
            .claude_ai_oauth;
        let profile: Profile =
            serde_json::from_str(r#"{"emailAddress":"dev@example.com"}"#).unwrap();
        let (account, detail) = make_account(NativeStatus::default(), Some(profile), oauth, None);
        let account = account.unwrap();
        assert!(!account.verified);
        assert_eq!(detail, "Saved sign-in");
        assert_eq!(account.plan.as_deref(), Some("Ultra"));
        let wire = serde_json::to_string(&account).unwrap();
        assert!(!wire.contains("dummy-access-token") && !wire.contains("dummy-refresh-token"));
    }

    #[test]
    fn a_partial_or_oversized_file_is_distinct_from_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        assert!(read_json::<CredentialsFile>(&path, 16).unwrap().is_none());
        std::fs::write(&path, b"{\"claudeAiOauth\":").unwrap();
        assert_eq!(
            read_json::<CredentialsFile>(&path, 64)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            read_json::<CredentialsFile>(&path, 8).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn a_plan_without_an_identity_has_no_quota_scope() {
        let status: NativeStatus = serde_json::from_str(
            r#"{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"pro"}"#,
        )
        .unwrap();
        let account = make_account(status, None, None, None).0.unwrap();
        assert_eq!(account.plan.as_deref(), Some("Pro"));
        assert!(account.id.is_none());
    }

    #[test]
    fn sign_out_and_api_auth_do_not_inherit_subscription_details() {
        let profile = || Profile {
            email_address: Some("dev@example.com".into()),
            ..Profile::default()
        };
        let oauth = || OAuth {
            subscription_type: Some("pro".into()),
            ..OAuth::default()
        };
        let signed_out = NativeStatus {
            logged_in: Some(false),
            ..NativeStatus::default()
        };
        assert_eq!(
            make_account(signed_out, Some(profile()), Some(oauth()), None),
            (None, SIGN_IN)
        );

        let api_key: NativeStatus = serde_json::from_str(
            r#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty"}"#,
        )
        .unwrap();
        let api = make_account(api_key, Some(profile()), Some(oauth()), None)
            .0
            .unwrap();
        assert_eq!(api.method, ClaudeAuthMethod::ApiKey);
        assert!(api.email.is_none() && api.plan.is_none());
    }
}
