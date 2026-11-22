//! One-shot display and browser operations inside the guest.

mod session;
mod state;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::model::{OperationState, StepState};
use crate::ops::{Operation, OperationManager, Step, StepCtx};
use crate::paths::AppPaths;
use crate::store::Store;
use crate::{Error, Result, fsx};

use self::session::{GuestSession, Session, verify_output};
use self::state::{
    Environment, LEGACY_FILES, Output, Saved, apply_profile, json_bytes, launch_profile_changed,
    native, profile, read_optional, restore_profile, sway_config, validate_request,
};

const INJECT: &str = include_str!("../../../vm/assets/browser_inject.js");

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Request {
    Status,
    BrowserStatus,
    Apply {
        width: u32,
        height: u32,
        scale: f64,
        zoom: u32,
    },
    Restore,
    SyncEnvironment {
        environment: Environment,
    },
}

/// Read one JSON request from stdin and write one response to stdout. This
/// runs directly as the SSH user regardless of the daemon service's state.
pub fn run(paths: &AppPaths, input: impl Read, mut output: impl Write) -> Result<()> {
    let request = serde_json::from_reader(input)?;
    let files = Files {
        state: paths.home().join(".local/state/cabin/native-display"),
        sway: paths.home().join(".config/sway/config"),
        browser: PathBuf::from("/opt/vmctl-fp"),
    };
    let session = Arc::new(GuestSession::new(paths.home(), &files.state));
    let response = match request {
        Request::Status => files.status(session.as_ref())?,
        Request::BrowserStatus => serde_json::to_value(session.browser_status())?,
        request => {
            let _lock = files.lock()?;
            let ops = OperationManager::new(Arc::new(Store::open(&paths.database())?));
            files.change(session, &ops, request)?
        }
    };
    serde_json::to_writer(&mut output, &response)?;
    writeln!(output)?;
    Ok(())
}

#[derive(Clone)]
struct Files {
    state: PathBuf,
    sway: PathBuf,
    browser: PathBuf,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum FileKind {
    Sway,
    Config,
    State,
    Inject,
    // Retained only to restore snapshots from journals written by older versions.
    Controller,
}

impl FileKind {
    fn name(self) -> &'static str {
        match self {
            Self::Sway => "sway",
            Self::Config => "config",
            Self::State => "state",
            Self::Inject => "inject",
            Self::Controller => "controller",
        }
    }

    fn location(self, files: &Files) -> (PathBuf, Option<u32>) {
        match self {
            Self::Sway => (files.sway.clone(), None),
            Self::Config => (files.browser.join("config.json"), Some(0o644)),
            Self::State => (files.state.join("state.json"), None),
            Self::Inject => (files.browser.join("inject.js"), Some(0o644)),
            Self::Controller => (files.browser.join("controller.py"), Some(0o755)),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Pending {
    backup: String,
    output: Option<Output>,
    // Old journals imply the original three paths and a Chrome restart.
    files: Option<Vec<FileKind>>,
    restart_chrome: Option<bool>,
}

struct Change {
    kind: FileKind,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}

struct Transaction {
    files: Files,
    session: Arc<dyn Session>,
    changes: Vec<Change>,
    geometry: Option<(Output, Output)>,
    chrome: Option<(bool, bool)>,
    pending: Pending,
}

impl Files {
    fn lock(&self) -> Result<File> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.state)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.state.join("lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0 {
            let error = std::io::Error::last_os_error();
            return Err(if error.kind() == std::io::ErrorKind::WouldBlock {
                Error::Conflict(
                    "Another native display or browser environment change is in progress.".into(),
                )
            } else {
                error.into()
            });
        }
        Ok(lock)
    }

    fn pending(&self) -> PathBuf {
        self.state.join("pending.json")
    }

    fn read(&self, kind: FileKind) -> Result<Option<Vec<u8>>> {
        read_optional(&kind.location(self).0)
    }

    fn write(&self, session: &dyn Session, kind: FileKind, data: Option<&[u8]>) -> Result<()> {
        let (path, root_mode) = kind.location(self);
        match (data, root_mode) {
            (Some(data), Some(mode)) => session.install(&path, data, mode),
            (None, Some(_)) => session.remove(&path),
            (Some(data), None) => fsx::atomic_write(
                &path,
                data,
                matches!(kind, FileKind::State).then_some(0o600),
            ),
            (None, None) => fsx::restore(&path, None),
        }
    }

    fn status(&self, session: &dyn Session) -> Result<Value> {
        let saved = Saved::load(&self.state)?;
        let output = session.output()?;
        let (logical_width, logical_height) = output.logical_size();
        let chrome = session.chrome();
        Ok(json!({
            "enabled": saved.is_some(),
            "recoveryRequired": self.pending().exists(),
            "zoom": saved.map(|state| state.zoom),
            "width": output.width,
            "height": output.height,
            "scale": output.scale,
            "logicalWidth": logical_width,
            "logicalHeight": logical_height,
            "chromeWayland": chrome.ready && chrome.wayland,
        }))
    }

    fn change(
        &self,
        session: Arc<dyn Session>,
        ops: &OperationManager,
        request: Request,
    ) -> Result<Value> {
        if self.pending().exists() {
            if !matches!(request, Request::Restore) {
                return Err(Error::Conflict(
                    "An interrupted display change needs recovery first.".into(),
                ));
            }
            let (files, session) = (self.clone(), session.clone());
            execute(
                ops,
                "Recover browser display",
                crate::ops::FnStep::new(
                    "Recover display files and session",
                    move |_| files.recover(session.as_ref()),
                    |_| Ok(()),
                ),
            )?;
        }
        let saved = Saved::load(&self.state)?;
        if matches!(request, Request::Restore) && saved.is_none() {
            return self.status(session.as_ref());
        }
        let current = fs::read(self.browser.join("config.json"))?;
        let config = profile(&current, false)?;
        let before_chrome = session.chrome();
        let mut transaction = Transaction::new(self.clone(), session.clone());
        let sync = matches!(request, Request::SyncEnvironment { .. });
        let (next, next_state, stale) = match request {
            Request::Apply {
                width,
                height,
                scale,
                zoom,
            } => {
                validate_request(width, height, scale, zoom)?;
                if !before_chrome.ready {
                    return Err(Error::Unavailable(
                        "Start Chrome before changing the display.".into(),
                    ));
                }
                let sway = fs::read_to_string(&self.sway)?;
                let before = session.output()?;
                let target = Output {
                    width,
                    height,
                    scale,
                    ..before.clone()
                };
                let (baseline, original, stale) = if let Some(saved) = &saved {
                    saved.check(&current, &config, &sway)?;
                    (
                        saved.baseline.clone(),
                        saved.original(&current)?,
                        !saved.matches_hash(&current),
                    )
                } else if native(&config) {
                    return Err(Error::Conflict(
                        "Native display is already enabled without saved restore state.".into(),
                    ));
                } else {
                    (before.clone(), current.as_slice(), false)
                };
                transaction.stage(FileKind::Sway, Some(sway_config(&sway, Some(&target))?))?;
                transaction.geometry = Some((before, target.clone()));
                let (next, state) =
                    apply_profile(&current, config.clone(), &target, baseline, zoom, original)?;
                (next, Some(state), stale)
            }
            Request::Restore => {
                let saved = saved.as_ref().expect("restore needs saved state");
                let sway = fs::read_to_string(&self.sway)?;
                saved.check(&current, &config, &sway)?;
                let before = session.output()?;
                transaction.stage(FileKind::Sway, Some(sway_config(&sway, None)?))?;
                transaction.geometry = Some((before, saved.baseline.clone()));
                (
                    restore_profile(&current, config.clone(), saved.original(&current)?)?,
                    None,
                    !saved.matches_hash(&current),
                )
            }
            Request::SyncEnvironment { environment } => {
                let mut next = config.clone();
                environment.patch(&mut next)?;
                if next == config {
                    return Ok(json!({"changed": false}));
                }
                let next = json_bytes(&next)?;
                let next_state = if let Some(saved) = &saved {
                    let sway = fs::read_to_string(&self.sway)?;
                    saved.check(&current, &config, &sway)?;
                    Some(saved.sync(&current, &next, &sway, &environment)?)
                } else {
                    None
                };
                (next, next_state, false)
            }
            Request::Status | Request::BrowserStatus => unreachable!(),
        };
        let next_profile = profile(&next, false)?;
        let inject = injection(&next_profile)?;
        let current_inject = self.read(FileKind::Inject)?;
        let inject_changed = sync
            || current_inject
                .as_ref()
                .is_none_or(|current| !same_injection(current, &inject));
        if inject_changed {
            transaction.stage(FileKind::Inject, Some(inject))?;
        }
        transaction.stage(FileKind::Config, Some(next))?;
        transaction.stage(FileKind::State, next_state)?;
        let expect_wayland = native(&next_profile);
        if before_chrome.pid.is_some()
            && (stale
                || before_chrome.wayland != expect_wayland
                || launch_profile_changed(&config, &next_profile)
                || inject_changed)
        {
            transaction.chrome = Some((native(&config), expect_wayland));
        }
        let title = if sync {
            "Synchronize browser environment"
        } else {
            "Change browser display"
        };
        execute(ops, title, transaction)?;
        for file in LEGACY_FILES {
            let _ = fs::remove_file(self.state.join(file));
        }
        if sync {
            Ok(json!({"changed": true}))
        } else {
            self.status(session.as_ref())
        }
    }

    fn recover(&self, session: &dyn Session) -> Result<()> {
        let Some(bytes) = read_optional(&self.pending())? else {
            return Ok(());
        };
        let pending: Pending = serde_json::from_slice(&bytes)?;
        let backup = self.state.join(&pending.backup);
        if !pending.backup.starts_with("transaction-")
            || backup.parent() != Some(&self.state)
            || !fs::symlink_metadata(&backup).is_ok_and(|metadata| metadata.is_dir())
        {
            return Err(Error::invalid("The display recovery record is invalid."));
        }
        let legacy = pending.files.is_none();
        let kinds = pending
            .files
            .unwrap_or_else(|| vec![FileKind::Sway, FileKind::Config, FileKind::State]);
        let snapshots: Vec<_> = kinds
            .into_iter()
            .map(|kind| {
                let data = read_optional(&backup.join(kind.name()))?;
                if legacy && matches!(kind, FileKind::Sway | FileKind::Config) && data.is_none() {
                    return Err(Error::invalid("The display recovery record is incomplete."));
                }
                Ok((kind, data))
            })
            .collect::<Result<_>>()?;
        for (kind, data) in snapshots {
            if self.read(kind)? != data {
                self.write(session, kind, data.as_deref())?;
            }
        }
        if let Some(output) = &pending.output {
            let live = session.output()?;
            session.set_output(&live, output)?;
            verify_output(&session.output()?, output)?;
            session.verify_viewer(output)?;
        }
        if legacy || pending.restart_chrome.is_some() {
            let wayland = if let Some(wayland) = pending.restart_chrome {
                wayland
            } else {
                native(&profile(
                    &fs::read(self.browser.join("config.json"))?,
                    false,
                )?)
            };
            session.restart_chrome(wayland)?;
        }
        self.finish(&backup)
    }

    fn finish(&self, backup: &Path) -> Result<()> {
        fs::remove_file(self.pending())?;
        let _ = fs::remove_dir_all(backup);
        Ok(())
    }
}

impl Transaction {
    fn new(files: Files, session: Arc<dyn Session>) -> Self {
        Self {
            files,
            session,
            changes: Vec::new(),
            geometry: None,
            chrome: None,
            pending: Pending {
                backup: format!("transaction-{}", uuid::Uuid::new_v4().simple()),
                output: None,
                files: None,
                restart_chrome: None,
            },
        }
    }

    fn stage(&mut self, kind: FileKind, after: Option<Vec<u8>>) -> Result<bool> {
        let before = self.files.read(kind)?;
        if before == after {
            return Ok(false);
        }
        self.changes.push(Change {
            kind,
            before,
            after,
        });
        Ok(true)
    }
}

impl Step for Transaction {
    fn name(&self) -> String {
        "Update browser display and profile".into()
    }

    fn prepare(&mut self, _: &StepCtx<'_>) -> Result<()> {
        let backup = self.files.state.join(&self.pending.backup);
        let result = (|| {
            fs::DirBuilder::new().mode(0o700).create(&backup)?;
            for change in &self.changes {
                if let Some(data) = &change.before {
                    fsx::atomic_write(&backup.join(change.kind.name()), data, Some(0o600))?;
                }
            }
            self.pending.files = Some(self.changes.iter().map(|change| change.kind).collect());
            self.pending.output = self.geometry.as_ref().map(|(before, _)| before.clone());
            self.pending.restart_chrome = self.chrome.map(|(old, _)| old);
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(backup);
        }
        result
    }

    fn apply(&mut self, _: &StepCtx<'_>) -> Result<()> {
        if self.changes.is_empty()
            && self.chrome.is_none()
            && self
                .geometry
                .as_ref()
                .is_none_or(|(before, after)| before == after)
        {
            let _ = fs::remove_dir_all(self.files.state.join(&self.pending.backup));
            return Ok(());
        }
        fsx::atomic_write(
            &self.files.pending(),
            &json_bytes(&self.pending)?,
            Some(0o600),
        )?;
        for change in &self.changes {
            self.files
                .write(self.session.as_ref(), change.kind, change.after.as_deref())?;
        }
        if let Some((before, after)) = &self.geometry {
            self.session.set_output(before, after)?;
        }
        Ok(())
    }

    fn verify(&mut self, _: &StepCtx<'_>) -> Result<()> {
        if !self.files.pending().exists() {
            return Ok(());
        }
        for change in &self.changes {
            if self.files.read(change.kind)? != change.after {
                return Err(Error::Unavailable(format!(
                    "The browser {} file did not reload.",
                    change.kind.name()
                )));
            }
        }
        if let Some((_, target)) = &self.geometry {
            verify_output(&self.session.output()?, target)?;
            self.session.verify_viewer(target)?;
        }
        if let Some((_, expected)) = self.chrome {
            self.session.restart_chrome(expected)?;
        }
        self.files
            .finish(&self.files.state.join(&self.pending.backup))
    }

    fn rollback(&mut self, _: &StepCtx<'_>) -> Result<()> {
        self.files.recover(self.session.as_ref())
    }
}

fn execute(ops: &OperationManager, title: &str, step: impl Step + 'static) -> Result<()> {
    let record = ops.run(
        Operation::new("nativeDisplay", title)
            .key("native-display")
            .step(step),
    )?;
    if record.state == OperationState::Succeeded {
        return Ok(());
    }
    let mut message = record
        .error
        .unwrap_or_else(|| format!("{title} did not complete."));
    if let Some(step) = record
        .steps
        .iter()
        .find(|step| step.state == StepState::RollbackFailed)
        && let Some(error) = step.log.last()
    {
        message.push_str(&format!(" {error}"));
    }
    Err(Error::Unavailable(message))
}

fn injection(profile: &Value) -> Result<Vec<u8>> {
    let encoded = serde_json::to_string(profile)?
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    let script = INJECT.replacen("__VMCTL_BROWSER_PROFILE__", &encoded, 1);
    let extra = profile["extra_js"].as_str().unwrap_or_default();
    Ok(format!("{script}\n{extra}").into_bytes())
}

fn same_injection(left: &[u8], right: &[u8]) -> bool {
    // The controller replaces the embedded declaration with its loaded
    // profile. The rest of the generated script still needs to match.
    fn with_placeholder(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
        let start = bytes
            .windows(b"\n  const C = ".len())
            .position(|part| part == b"\n  const C = ")?;
        let end = start + 1 + bytes[start + 1..].iter().position(|byte| *byte == b'\n')?;
        Some((&bytes[..start], &bytes[end..]))
    }
    with_placeholder(left).is_some_and(|parts| Some(parts) == with_placeholder(right))
}

#[cfg(test)]
mod tests;
