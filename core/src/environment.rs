//! The VM's timezone and language as one configuration transaction.
//! (Networking is the Mac's: every VM connection goes through its gateway,
//! so nothing here configures a proxy.)

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::error::{Error, Result};
use crate::model::{EnvironmentReview, EnvironmentSpec, EnvironmentState, FieldState};
use crate::ops::{Operation, Step, StepCtx};
use crate::system::{CommandSpec, privileged};
use crate::util::now_millis;

const SETTING: &str = "environment";
const PRIVILEGED_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Committed {
    spec: EnvironmentSpec,
    applied_at: Option<i64>,
}

fn committed(ctx: &Context) -> Committed {
    ctx.store
        .setting(SETTING)
        .ok()
        .flatten()
        .unwrap_or_default()
}

pub fn committed_spec(ctx: &Context) -> EnvironmentSpec {
    committed(ctx).spec
}

// ------------------------------------------------------------ system facts

pub fn system_timezone(ctx: &Context) -> Option<String> {
    let spec = CommandSpec::new(["timedatectl", "show", "-p", "Timezone", "--value"])
        .timeout(Duration::from_secs(5));
    if let Ok(out) = ctx.system.check(&spec) {
        let value = out.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    timezone_from_files()
}

/// The configured zone from `/etc/timezone` or the `/etc/localtime` link,
/// without spawning anything (cheap enough for every clock sample).
pub fn timezone_from_files() -> Option<String> {
    std::fs::read_to_string("/etc/timezone")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .or_else(|| {
            let target = std::fs::read_link("/etc/localtime").ok()?;
            let text = target.to_string_lossy();
            text.split_once("zoneinfo/")
                .map(|(_, zone)| zone.to_string())
        })
}

pub fn system_language(ctx: &Context) -> Option<String> {
    let spec = CommandSpec::new(["localectl", "status"]).timeout(Duration::from_secs(5));
    if let Ok(out) = ctx.system.check(&spec) {
        for line in out.lines() {
            if let Some((_, rest)) = line.split_once("System Locale:")
                && let Some(lang) = rest
                    .split_whitespace()
                    .find_map(|part| part.strip_prefix("LANG="))
            {
                return Some(lang.to_string());
            }
        }
    }
    std::env::var("LANG").ok().filter(|lang| !lang.is_empty())
}

pub fn timezones(ctx: &Context) -> Vec<String> {
    let spec = CommandSpec::new(["timedatectl", "list-timezones"]).timeout(Duration::from_secs(5));
    let mut zones: Vec<String> = ctx
        .system
        .check(&spec)
        .map(|out| {
            out.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    if zones.is_empty() {
        zones.push("UTC".into());
    }
    zones
}

/// `en_US.utf8` → `en_US.UTF-8`.
fn normalize_locale(raw: &str) -> String {
    match raw.split_once('.') {
        Some((base, codeset))
            if codeset.eq_ignore_ascii_case("utf8") || codeset.eq_ignore_ascii_case("utf-8") =>
        {
            format!("{base}.UTF-8")
        }
        _ => raw.to_string(),
    }
}

/// Locales generated on this machine (only these can be applied).
pub fn languages(ctx: &Context) -> Vec<String> {
    let spec = CommandSpec::new(["locale", "-a"]).timeout(Duration::from_secs(5));
    let mut list: Vec<String> = ctx
        .system
        .check(&spec)
        .map(|out| {
            out.lines()
                .map(str::trim)
                .filter(|line| line.to_ascii_lowercase().contains("utf"))
                .map(normalize_locale)
                .collect()
        })
        .unwrap_or_default();
    list.sort();
    list.dedup();
    list
}

// ------------------------------------------------------------------ state

pub fn state(ctx: &Context) -> EnvironmentState {
    let committed = committed(ctx);
    let (timezone, language) = std::thread::scope(|scope| {
        let timezone = scope.spawn(|| system_timezone(ctx));
        let language = system_language(ctx);
        (timezone.join().unwrap_or(None), language)
    });
    EnvironmentState {
        committed: committed.spec,
        applied_at: committed.applied_at,
        system_timezone: timezone,
        system_language: language,
    }
}

/// Environment variables for child processes (sessions, installs).
pub fn child_env(ctx: &Context) -> Vec<(String, String)> {
    vec![("LANG".to_string(), committed_spec(ctx).language)]
}

// ----------------------------------------------------------------- review

fn validate(spec: &EnvironmentSpec, languages: &[String]) -> Result<()> {
    if spec.timezone.trim().is_empty() {
        return Err(Error::invalid("Choose a timezone"));
    }
    if !languages.is_empty() && !languages.iter().any(|lang| lang == &spec.language) {
        return Err(Error::invalid(format!(
            "The locale {} is not generated on this machine. Run `sudo locale-gen {}` or pick another language.",
            spec.language, spec.language
        )));
    }
    Ok(())
}

/// Field states, the change list and any blocking problem for a draft.
pub fn review(ctx: &Context, draft: &EnvironmentSpec) -> EnvironmentReview {
    let committed = committed_spec(ctx);
    let languages = languages(ctx);
    let system_tz = system_timezone(ctx);
    let system_lang = system_language(ctx);

    let fields = vec![
        FieldState {
            field: "timezone".into(),
            enabled: true,
            reason: None,
            suggestion: system_tz
                .clone()
                .filter(|tz| tz != &draft.timezone && draft.timezone == "UTC"),
        },
        FieldState {
            field: "language".into(),
            enabled: !languages.is_empty(),
            reason: languages
                .is_empty()
                .then(|| "Could not list installed locales (`locale -a`)".into()),
            suggestion: (draft.language != "en_US.UTF-8"
                && languages.iter().any(|l| l == "en_US.UTF-8"))
            .then(|| "en_US.UTF-8".into()),
        },
    ];

    let mut changes = Vec::new();
    let current_tz = system_tz.unwrap_or(committed.timezone.clone());
    if current_tz != draft.timezone {
        changes.push(format!("Timezone: {current_tz} → {}", draft.timezone));
    }
    let current_lang = system_lang.unwrap_or(committed.language.clone());
    if current_lang != draft.language {
        changes.push(format!("Language: {current_lang} → {}", draft.language));
    }

    EnvironmentReview {
        fields,
        changes,
        blocked: validate(draft, &languages)
            .err()
            .map(|error| error.to_string()),
        timezones: timezones(ctx),
        languages,
    }
}

// ------------------------------------------------------------------ steps

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn environment_script(spec: &EnvironmentSpec) -> String {
    let mut text = String::from(
        "# Generated by cabin. `source` this file to use the app's environment.\n",
    );
    text.push_str(&format!("export LANG={}\n", shell_quote(&spec.language)));
    text.push_str(&format!("export TZ={}\n", shell_quote(&spec.timezone)));
    // Traffic is routed by the Mac's gateway; clear proxies from older setups.
    text.push_str("unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy NO_PROXY no_proxy\n");
    text
}

struct ValidateStep {
    ctx: Context,
    spec: EnvironmentSpec,
}

impl Step for ValidateStep {
    fn name(&self) -> String {
        "Validate settings".into()
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        validate(&self.spec, &languages(&self.ctx))
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        ctx.log("Settings are valid");
        Ok(())
    }
}

struct ScriptStep {
    ctx: Context,
    spec: EnvironmentSpec,
    backup: Option<std::path::PathBuf>,
}

impl Step for ScriptStep {
    fn name(&self) -> String {
        "Environment file".into()
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        self.backup = crate::fsx::backup(
            &self.ctx.paths.environment_script(),
            &self.ctx.paths.backups().join("environment"),
        )?;
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let path = self.ctx.paths.environment_script();
        crate::fsx::atomic_write(
            &path,
            environment_script(&self.spec).as_bytes(),
            Some(0o644),
        )?;
        ctx.log(format!("Wrote {}", path.display()));
        Ok(())
    }

    fn rollback(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        crate::fsx::restore(&self.ctx.paths.environment_script(), self.backup.as_deref())
    }
}

struct TimezoneStep {
    ctx: Context,
    target: String,
    previous: Option<String>,
}

impl Step for TimezoneStep {
    fn name(&self) -> String {
        format!("Timezone → {}", self.target)
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        self.previous = system_timezone(&self.ctx);
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        ctx.log("Changing the system timezone");
        self.ctx.system.check(
            &privileged(["timedatectl", "set-timezone", self.target.as_str()])
                .timeout(PRIVILEGED_TIMEOUT),
        )?;
        Ok(())
    }

    fn verify(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        match system_timezone(&self.ctx) {
            Some(zone) if zone == self.target => {
                ctx.log(format!("System timezone is now {zone}"));
                Ok(())
            }
            other => Err(Error::Internal(format!(
                "Timezone is {} after the change",
                other.unwrap_or_else(|| "unknown".into())
            ))),
        }
    }

    fn rollback(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        match &self.previous {
            Some(previous) if system_timezone(&self.ctx).as_ref() != Some(previous) => self
                .ctx
                .system
                .check(
                    &privileged(["timedatectl", "set-timezone", previous.as_str()])
                        .timeout(PRIVILEGED_TIMEOUT),
                )
                .map(|_| ()),
            _ => Ok(()),
        }
    }
}

struct LanguageStep {
    ctx: Context,
    target: String,
    previous: Option<String>,
}

impl Step for LanguageStep {
    fn name(&self) -> String {
        format!("Language → {}", self.target)
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        self.previous = system_language(&self.ctx);
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        ctx.log("Changing the system locale");
        self.ctx.system.check(
            &privileged([
                "localectl".to_string(),
                "set-locale".into(),
                format!("LANG={}", self.target),
            ])
            .timeout(PRIVILEGED_TIMEOUT),
        )?;
        ctx.log(
            "New sessions use this language now; desktop apps pick it up after you log in again",
        );
        Ok(())
    }

    fn verify(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        match system_language(&self.ctx) {
            Some(lang) if lang == self.target => Ok(()),
            other => Err(Error::Internal(format!(
                "System locale is {} after the change",
                other.unwrap_or_else(|| "unknown".into())
            ))),
        }
    }

    fn rollback(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        match &self.previous {
            Some(previous) if system_language(&self.ctx).as_ref() != Some(previous) => self
                .ctx
                .system
                .check(
                    &privileged([
                        "localectl".to_string(),
                        "set-locale".into(),
                        format!("LANG={previous}"),
                    ])
                    .timeout(PRIVILEGED_TIMEOUT),
                )
                .map(|_| ()),
            _ => Ok(()),
        }
    }
}

struct CommitStep {
    ctx: Context,
    spec: EnvironmentSpec,
    previous: Option<Committed>,
}

impl Step for CommitStep {
    fn name(&self) -> String {
        "Save environment".into()
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        self.previous = self.ctx.store.setting(SETTING)?;
        Ok(())
    }

    fn apply(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        self.ctx.store.set_setting(
            SETTING,
            &Committed {
                spec: self.spec.clone(),
                applied_at: Some(now_millis()),
            },
        )
    }

    fn rollback(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        match &self.previous {
            Some(previous) => self.ctx.store.set_setting(SETTING, previous),
            None => self.ctx.store.remove_setting(SETTING),
        }
    }
}

/// Build the single "Apply environment" operation for `spec`.
pub fn apply_operation(ctx: &Context, spec: EnvironmentSpec) -> Result<Operation> {
    let mut operation = Operation::new("environment.apply", "Apply environment")
        .key("environment")
        .request(serde_json::to_value(&spec)?)
        .step(ValidateStep {
            ctx: ctx.clone(),
            spec: spec.clone(),
        })
        .step(ScriptStep {
            ctx: ctx.clone(),
            spec: spec.clone(),
            backup: None,
        });
    if system_timezone(ctx).as_deref() != Some(spec.timezone.as_str()) {
        operation = operation.step(TimezoneStep {
            ctx: ctx.clone(),
            target: spec.timezone.clone(),
            previous: None,
        });
    }
    if system_language(ctx).as_deref() != Some(spec.language.as_str()) {
        operation = operation.step(LanguageStep {
            ctx: ctx.clone(),
            target: spec.language.clone(),
            previous: None,
        });
    }
    Ok(operation.step(CommitStep {
        ctx: ctx.clone(),
        spec,
        previous: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OperationState;
    use crate::ops::OperationManager;
    use crate::paths::AppPaths;
    use crate::store::Store;
    use crate::system::fake::{FakeSystem, fail, ok};
    use std::sync::{Arc, Mutex};

    struct Machine {
        tz: Arc<Mutex<String>>,
        lang: Arc<Mutex<String>>,
        fake: Arc<FakeSystem>,
    }

    /// A fake OS whose timezone/locale can be read and changed; `localectl
    /// set-locale` fails when `locale_fails` is true.
    fn machine(locale_fails: bool) -> Machine {
        let fake = Arc::new(FakeSystem::default());
        let tz = Arc::new(Mutex::new("UTC".to_string()));
        let lang = Arc::new(Mutex::new("en_US.UTF-8".to_string()));
        let (tz_read, tz_write) = (tz.clone(), tz.clone());
        let (lang_read, lang_write) = (lang.clone(), lang.clone());
        fake.on(move |spec| {
            let mut argv: Vec<&str> = spec.argv.iter().map(String::as_str).collect();
            if argv.first() == Some(&"sudo") {
                argv.drain(..2);
            }
            match argv.as_slice() {
                ["timedatectl", "show", ..] => Some(ok(&tz_read.lock().unwrap())),
                ["timedatectl", "set-timezone", zone] => {
                    *tz_write.lock().unwrap() = zone.to_string();
                    Some(ok(""))
                }
                ["timedatectl", "list-timezones"] => Some(ok("UTC\nAsia/Tokyo\n")),
                ["locale", "-a"] => Some(ok("C.utf8\nen_US.utf8\nde_DE.utf8\n")),
                ["localectl", "status"] => Some(ok(&format!(
                    "   System Locale: LANG={}\n",
                    lang_read.lock().unwrap()
                ))),
                ["localectl", "set-locale", arg] => {
                    if locale_fails {
                        Some(fail("Access denied"))
                    } else {
                        *lang_write.lock().unwrap() = arg.trim_start_matches("LANG=").to_string();
                        Some(ok(""))
                    }
                }
                _ => None,
            }
        });
        Machine { tz, lang, fake }
    }

    fn context(dir: &std::path::Path, system: Arc<FakeSystem>) -> Context {
        Context {
            paths: AppPaths::new(dir.join("data"), dir.join("home")),
            store: Arc::new(Store::in_memory().unwrap()),
            system,
        }
    }

    fn draft() -> EnvironmentSpec {
        EnvironmentSpec {
            timezone: "Asia/Tokyo".into(),
            language: "de_DE.UTF-8".into(),
        }
    }

    #[test]
    fn apply_changes_everything_and_commits() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let machine = machine(false);
        let ctx = context(dir.path(), machine.fake.clone());
        let ops = OperationManager::new(ctx.store.clone());
        let record = ops.run(apply_operation(&ctx, draft())?)?;
        assert_eq!(record.state, OperationState::Succeeded, "{record:?}");
        assert_eq!(*machine.tz.lock().unwrap(), "Asia/Tokyo");
        assert_eq!(*machine.lang.lock().unwrap(), "de_DE.UTF-8");
        assert_eq!(committed_spec(&ctx).timezone, "Asia/Tokyo");
        let script = std::fs::read_to_string(ctx.paths.environment_script())?;
        assert!(script.contains("export LANG='de_DE.UTF-8'"));
        Ok(())
    }

    #[test]
    fn locale_failure_rolls_back_timezone_and_commit() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let machine = machine(true);
        let ctx = context(dir.path(), machine.fake.clone());
        let ops = OperationManager::new(ctx.store.clone());
        let record = ops.run(apply_operation(&ctx, draft())?)?;
        assert_eq!(record.state, OperationState::RolledBack, "{record:?}");
        assert_eq!(*machine.tz.lock().unwrap(), "UTC", "timezone restored");
        assert_eq!(committed_spec(&ctx), EnvironmentSpec::default());
        assert!(!ctx.paths.environment_script().exists(), "script removed");
        assert!(
            machine
                .fake
                .calls()
                .iter()
                .any(|call| call.ends_with("timedatectl set-timezone UTC"))
        );
        Ok(())
    }

    #[test]
    fn review_blocks_a_locale_that_is_not_generated() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path(), machine(false).fake);
        let mut spec = draft();
        spec.language = "ko_KR.UTF-8".into();
        let review = review(&ctx, &spec);
        assert!(
            review
                .blocked
                .as_deref()
                .unwrap()
                .contains("ko_KR.UTF-8 is not generated")
        );
        assert!(
            review
                .changes
                .iter()
                .any(|c| c.starts_with("Timezone: UTC → Asia/Tokyo"))
        );
        assert_eq!(review.fields.len(), 2, "timezone and language only");
        Ok(())
    }

    #[test]
    fn sessions_and_the_script_carry_no_proxy() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path(), machine(false).fake);
        assert_eq!(
            child_env(&ctx),
            vec![("LANG".to_string(), "en_US.UTF-8".to_string())]
        );
        let script = environment_script(&draft());
        assert!(script.contains("unset HTTP_PROXY") && !script.contains("export HTTP_PROXY"));
        Ok(())
    }
}
