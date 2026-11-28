use std::path::PathBuf;

mod account;
pub(super) use account::inspect as inspect_account;

use super::extensions::{McpCodec, json};
use super::{AgentAdapter, AgentSpec};
use crate::context::Context;
use crate::error::{Error, Result};
use crate::model::{AgentKind, ClaudeAuthMethod, ClaudeLogin, McpServer};
use crate::util::LockExt;
use serde_json::Value;

pub struct Claude;

static SPEC: AgentSpec = AgentSpec {
    kind: AgentKind::Claude,
    name: "Claude Code",
    binary: "claude",
    package: "@anthropic-ai/claude-code",
    config: ".claude/settings.json",
    model_key: "model",
    auth_files: &[".claude/.credentials.json"],
    auth_env: &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
};

impl AgentAdapter for Claude {
    fn spec(&self) -> &'static AgentSpec {
        &SPEC
    }

    fn login_args(&self) -> &'static [&'static str] {
        &["auth", "login"]
    }

    fn mcp(&self) -> Option<&'static dyn McpCodec> {
        Some(&Mcp)
    }

    fn skills_dir(&self, ctx: &Context) -> PathBuf {
        ctx.paths.home().join(".claude/skills")
    }

    fn project_skills_dir(&self) -> &'static str {
        ".claude/skills"
    }
}

/// Ask Claude Code's own non-interactive /usage command for the current
/// account. It is invoked only on request, without tools or saved sessions.
pub(crate) fn usage(ctx: &Context, account_id: Option<&str>) -> Result<crate::model::ClaudeUsage> {
    let binary = super::find_binary(ctx, AgentKind::Claude)
        .ok_or_else(|| Error::Unavailable("Claude Code is not installed".into()))?;
    usage_for(
        ctx,
        &binary,
        &account::AccountFiles::new(ctx.paths.home()),
        account_id,
    )
}

fn usage_for(
    ctx: &Context,
    binary: &std::path::Path,
    files: &account::AccountFiles,
    account_id: Option<&str>,
) -> Result<crate::model::ClaudeUsage> {
    use std::time::Duration;
    static USAGE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = USAGE.locked();
    let check_account = || {
        let Some(id) = account_id else { return Ok(()) };
        match account::inspect_at(ctx, Some(binary), files).0 {
            ClaudeLogin::Active { account }
                if account.id.as_deref() == Some(id)
                    && account.method == ClaudeAuthMethod::Subscription
                    && !account.usage_based =>
            {
                Ok(())
            }
            ClaudeLogin::Unavailable => Err(Error::Unavailable(
                "Could not check the active Claude account. Limits will retry automatically."
                    .into(),
            )),
            _ => Err(Error::Conflict(
                "Claude Code’s active account changed. Its limits will update automatically."
                    .into(),
            )),
        }
    };
    check_account()?;
    let mut command = crate::system::CommandSpec::new([
        binary.display().to_string(),
        "-p".into(),
        "/usage".into(),
        "--output-format".into(),
        "json".into(),
        "--verbose".into(),
        "--tools".into(),
        "".into(),
        "--strict-mcp-config".into(),
        "--setting-sources".into(),
        "".into(),
        "--settings".into(),
        r#"{"disableDeepLinkRegistration":"disable"}"#.into(),
        "--no-session-persistence".into(),
    ])
    .env([
        ("DISABLE_TELEMETRY".into(), "1".into()),
        ("DISABLE_ERROR_REPORTING".into(), "1".into()),
        ("DISABLE_AUTOUPDATER".into(), "1".into()),
    ])
    .timeout(Duration::from_secs(30))
    .output_limit(2 * 1024 * 1024);
    command.cwd = Some(ctx.paths.data().display().to_string());
    files.apply(&mut command);
    let output = ctx.system.run(&command)?;
    if !output.success() {
        return Err(Error::Unavailable(
            "Claude Code could not read usage. Check its sign-in and try again.".into(),
        ));
    }
    check_account()?;
    parse_usage(&output.stdout)
}

fn parse_usage(text: &str) -> Result<crate::model::ClaudeUsage> {
    use crate::model::{ClaudeUsage, ClaudeUsageWindow};
    let value: Value = serde_json::from_str(text).map_err(|_| {
        Error::Unavailable("Claude Code returned an unreadable usage report".into())
    })?;
    let messages: Vec<&Value> = match &value {
        Value::Array(messages) => messages.iter().collect(),
        Value::Object(_) => vec![&value],
        _ => {
            return Err(Error::Unavailable(
                "Claude Code returned an unreadable usage report".into(),
            ));
        }
    };
    let result = messages
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("result"));
    if result.is_some_and(|item| item.get("is_error").and_then(Value::as_bool) == Some(true)) {
        return Err(Error::Unavailable(
            "Claude Code could not read usage. Check its sign-in and try again.".into(),
        ));
    }
    let mut windows: Vec<ClaudeUsageWindow> = Vec::new();
    let mut no_limits = false;
    for message in &messages {
        let Some(rows) = message
            .pointer("/usage_report/rate_limits/limits")
            .and_then(Value::as_array)
        else {
            continue;
        };
        no_limits |= rows.is_empty();
        for row in rows {
            let label = match row.get("kind").and_then(Value::as_str) {
                Some("session") => "5 hours".to_string(),
                Some("weekly_all") => "7 days".to_string(),
                Some("weekly_scoped") => match row
                    .pointer("/scope/model/display_name")
                    .and_then(Value::as_str)
                {
                    Some(model) if !model.trim().is_empty() => format!("7 days · {}", model.trim()),
                    _ => continue,
                },
                _ => continue,
            };
            let Some(percent) = row
                .get("percent")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
            else {
                continue;
            };
            let resets_at = row
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
                .map(|date| date.timestamp_millis());
            if !windows.iter().any(|window| window.label == label) {
                windows.push(ClaudeUsageWindow {
                    label,
                    used_percent: percent.clamp(0.0, 100.0),
                    resets_at,
                    reset_text: None,
                });
            }
        }
    }
    // Older versions print the windows in the result text instead of rows.
    if let Some(result_text) = result
        .and_then(|item| item.get("result"))
        .and_then(Value::as_str)
    {
        for line in result_text.lines().map(str::trim) {
            let Some((heading, rest)) = line.split_once(':') else {
                continue;
            };
            let label = if heading == "Current session" {
                "5 hours".to_string()
            } else if heading == "Current week" || heading == "Current week (all models)" {
                "7 days".to_string()
            } else if let Some(scope) = heading
                .strip_prefix("Current week (")
                .and_then(|s| s.strip_suffix(')'))
            {
                format!("7 days · {scope}")
            } else {
                continue;
            };
            let Some((percent, rest)) = rest.trim().split_once("% used") else {
                continue;
            };
            let Ok(percent) = percent.trim().parse::<f64>() else {
                continue;
            };
            if !percent.is_finite() {
                continue;
            }
            let reset_text = rest
                .trim()
                .strip_prefix('·')
                .map(str::trim)
                .and_then(|s| s.strip_prefix("resets "))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let span = if label == "5 hours" { 5 } else { 7 * 24 };
            let resets_at = reset_text
                .as_deref()
                .and_then(|text| parse_reset(text, chrono::Utc::now(), span));
            if let Some(window) = windows.iter_mut().find(|window| window.label == label) {
                if window.resets_at.is_none() {
                    window.resets_at = resets_at;
                    window.reset_text = reset_text;
                }
            } else {
                windows.push(ClaudeUsageWindow {
                    label,
                    used_percent: percent.clamp(0.0, 100.0),
                    resets_at,
                    reset_text,
                });
            }
        }
    }
    if windows.is_empty() && !no_limits {
        return Err(Error::Unavailable(
            "Claude Code did not report usage windows. Check its sign-in and try again.".into(),
        ));
    }
    Ok(ClaudeUsage {
        windows,
        no_limits,
        refreshed_at: crate::util::now_millis(),
    })
}

// Claude's older /usage returns dates without a year, with an IANA zone.
// Resolve only dates that fit this window; preserve unrecognized text as-is.
fn parse_reset(text: &str, now: chrono::DateTime<chrono::Utc>, span_hours: i64) -> Option<i64> {
    use chrono::{Datelike, Duration, NaiveDateTime, NaiveTime, TimeZone};
    if let Ok(date) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(date.timestamp_millis());
    }
    let (date, zone) = text.trim().rsplit_once(" (")?;
    let zone: chrono_tz::Tz = zone.strip_suffix(')')?.parse().ok()?;
    let local = now.with_timezone(&zone);
    let mut candidates = Vec::new();
    let mut push = |date: NaiveDateTime| {
        let local = zone.from_local_datetime(&date);
        if let Some(time) = local.earliest() {
            candidates.push(time.with_timezone(&chrono::Utc));
        }
        if let Some(time) = local.latest() {
            candidates.push(time.with_timezone(&chrono::Utc));
        }
    };
    let date = date.replace("AM", "am").replace("PM", "pm");
    for format in [
        "%b %e, %Y at %l:%M%P",
        "%b %e, %Y at %l%P",
        "%b %e, %Y, %l:%M%P",
        "%b %e, %Y, %l%P",
    ] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(&date, format) {
            push(parsed);
        }
    }
    for year in (local.year() - 1)..=(local.year() + 1) {
        let with_year = format!("{date} {year}");
        for format in [
            "%b %e at %l:%M%P %Y",
            "%b %e at %l%P %Y",
            "%b %e, %l:%M%P %Y",
            "%b %e, %l%P %Y",
        ] {
            if let Ok(parsed) = NaiveDateTime::parse_from_str(&with_year, format) {
                push(parsed);
            }
        }
    }
    for format in ["%l:%M%P", "%l%P"] {
        if let Ok(time) = NaiveTime::parse_from_str(&date, format) {
            for day in -2..=2 {
                if let Some(date) = local.date_naive().checked_add_signed(Duration::days(day)) {
                    push(date.and_time(time));
                }
            }
        }
    }
    candidates.sort();
    candidates
        .iter()
        .find(|time| **time >= now && **time <= now + Duration::hours(span_hours + 2))
        .or_else(|| {
            candidates
                .iter()
                .rev()
                .find(|time| **time < now && **time >= now - Duration::days(1))
        })
        .map(|time| time.timestamp_millis())
}

/// User-scope servers: `mcpServers` in the agent's home JSON file, entries
/// `{type: stdio, command, args, env}` or `{type: http|sse, url, headers}`.
/// The format has no "disabled" flag.
struct Mcp;

const MCP_KEY: &str = "mcpServers";

impl McpCodec for Mcp {
    fn path(&self, ctx: &Context) -> PathBuf {
        ctx.paths.home().join(".claude.json")
    }

    fn can_disable(&self) -> bool {
        false
    }

    fn ids(&self, text: &str) -> Result<Vec<String>> {
        json::ids(text, MCP_KEY)
    }

    fn shelve(&self, text: &str, name: &str) -> Result<Option<Value>> {
        let mut root = json::parse(Some(text))?;
        Ok(json::table(&mut root, MCP_KEY)?.shift_remove(name))
    }

    fn unshelve(&self, text: Option<&str>, name: &str, entry: &Value) -> Result<String> {
        let mut root = json::parse(text)?;
        json::table(&mut root, MCP_KEY)?.insert(name.into(), entry.clone());
        json::finish(&root)
    }

    fn read(&self, text: &str) -> Result<Vec<McpServer>> {
        json::standard_servers(text, MCP_KEY, false)
    }

    fn upsert(
        &self,
        text: Option<&str>,
        server: &McpServer,
        previous: Option<&str>,
    ) -> Result<String> {
        json::update_standard(text, MCP_KEY, server, previous, false)
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

#[cfg(test)]
mod usage_tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn usage_invokes_only_the_noninteractive_usage_command() -> Result<()> {
        use crate::{
            paths::AppPaths,
            store::Store,
            system::fake::{FakeSystem, ok},
        };
        use std::{os::unix::fs::PermissionsExt, sync::Arc};
        let dir = tempfile::tempdir()?;
        let paths = AppPaths::new(dir.path().join("data"), dir.path().join("home"));
        let bin = paths.home().join(".local/bin/claude");
        std::fs::create_dir_all(bin.parent().unwrap())?;
        std::fs::write(&bin, "stub")?;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700))?;
        let system = Arc::new(FakeSystem::default());
        system.on(|spec| {
            assert_eq!(&spec.argv[1..3], &["-p", "/usage"]);
            assert!(
                spec.argv
                    .iter()
                    .any(|arg| arg == "--no-session-persistence")
            );
            assert!(spec.argv.iter().any(|arg| arg == "--strict-mcp-config"));
            assert_eq!(spec.timeout, Some(std::time::Duration::from_secs(30)));
            Some(ok(
                r#"{"type":"result","result":"Current session: 13% used"}"#,
            ))
        });
        let ctx = Context {
            paths,
            store: Arc::new(Store::in_memory()?),
            system,
        };
        assert_eq!(usage(&ctx, None)?.windows[0].used_percent, 13.0);
        Ok(())
    }

    #[test]
    fn usage_rejects_a_seat_change_during_the_read() -> Result<()> {
        use crate::{
            paths::AppPaths,
            store::Store,
            system::fake::{FakeSystem, ok},
        };
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let dir = tempfile::tempdir()?;
        let paths = AppPaths::new(dir.path().join("data"), dir.path().join("home"));
        let binary = paths.home().join("claude-stub");
        let files = account::AccountFiles::at(paths.home(), None);
        let system = Arc::new(FakeSystem::default());
        let switched = AtomicBool::new(false);
        system.on(move |spec| Some(match &spec.argv[1..] {
            args if args == ["auth", "status", "--json"] => {
                let org = if switched.load(Ordering::Relaxed) { "other-org" } else { "initial-org" };
                ok(&serde_json::json!({"loggedIn": true, "authMethod": "claude.ai", "email": "dev@example.com", "orgId": org}).to_string())
            }
            args if args.starts_with(&["-p".into(), "/usage".into()]) => {
                switched.store(true, Ordering::Relaxed);
                ok(r#"{"type":"result","result":"Current session: 13% used"}"#)
            }
            _ => panic!("unexpected Claude probe"),
        }));
        let ctx = Context {
            paths,
            store: Arc::new(Store::in_memory()?),
            system: system.clone(),
        };
        let (ClaudeLogin::Active { account }, _) = account::inspect_at(&ctx, Some(&binary), &files)
        else {
            panic!("initial account should be active");
        };
        assert!(matches!(
            usage_for(&ctx, &binary, &files, account.id.as_deref()),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            usage_for(&ctx, &binary, &files, account.id.as_deref()),
            Err(Error::Conflict(_))
        ));
        assert_eq!(
            system
                .calls()
                .iter()
                .filter(|call| call.contains("/usage"))
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn reads_structured_and_text_windows() {
        let report = serde_json::json!([
            {"type":"assistant", "usage_report":{"rate_limits":{"limits":[
                {"kind":"session", "percent":37.5, "resets_at":"2026-11-19T15:00:00Z"},
                {"kind":"weekly_scoped", "percent":4, "scope":{"model":{"display_name":"Sonnet"}}},
                {"kind":"weekly_scoped", "percent":9, "scope":{"surface":{"display_name":"Other"}}}
            ]}}},
            {"type":"result", "is_error":false, "result":"Current session: 12% used\nCurrent week (Sonnet): 4% used · resets 2026-11-19T15:00:00Z\nCurrent week (all models): 23% used · resets 3pm (UTC)"}
        ]);
        let usage = parse_usage(&report.to_string()).unwrap();
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].used_percent, 37.5);
        assert_eq!(
            usage.windows[0].resets_at,
            Some(
                chrono::Utc
                    .with_ymd_and_hms(2026, 11, 19, 15, 0, 0)
                    .unwrap()
                    .timestamp_millis()
            )
        );
        assert_eq!(usage.windows[1].label, "7 days · Sonnet");
        assert_eq!(usage.windows[1].resets_at, usage.windows[0].resets_at);
        assert_eq!(usage.windows[2].used_percent, 23.0);
        assert_eq!(usage.windows[2].reset_text.as_deref(), Some("3pm (UTC)"));
        assert!(!usage.no_limits);
        let empty = serde_json::json!([{"usage_report":{"rate_limits":{"limits":[]}}},{"type":"result","result":"No meters"}]);
        assert!(parse_usage(&empty.to_string()).unwrap().no_limits);
        assert!(parse_usage(r#"{"type":"result","result":"temporarily unavailable"}"#).is_err());
        assert!(parse_usage(r#"{"type":"result","is_error":true,"result":"signed out"}"#).is_err());
    }

    #[test]
    fn reads_reset_dates_in_the_reported_zone() {
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 11, 19, 12, 0, 0)
            .unwrap();
        let expected = chrono::Utc
            .with_ymd_and_hms(2026, 11, 19, 15, 30, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            parse_reset("11:30pm (Asia/Shanghai)", now, 5),
            Some(expected)
        );
        assert_eq!(
            parse_reset("Nov 19 at 11:30pm (Asia/Shanghai)", now, 5),
            Some(expected)
        );
        assert_eq!(
            parse_reset("Nov 19, 11:30pm (Asia/Shanghai)", now, 5),
            Some(expected)
        );
        assert_eq!(parse_reset("11:30pm (Unknown/Zone)", now, 5), None);
    }
}
