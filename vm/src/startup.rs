//! Timed startup phases and bounded log tails for one launch or attachment.

use crate::util::LockExt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::Config;
use crate::util::log_line;

static NEXT_LOG: AtomicU64 = AtomicU64::new(0);
const TAIL_LINES: usize = 40;
const TAIL_BYTES: u64 = 64 * 1024;

type Log = Arc<Mutex<File>>;

#[derive(Clone, Copy)]
pub enum Phase {
    Gateway,
    Preflight,
    Install,
    Qemu,
    Boot,
    Guest,
    Daemon,
    Library,
    Network,
}

const PHASES: [(&str, &str); 9] = [
    ("gateway", "Open the network gateway"),
    ("preflight", "Check the configuration"),
    ("install", "Verify QEMU and the disk"),
    ("qemu", "Launch the virtual machine"),
    ("boot", "Boot Linux"),
    ("guest", "Mount folders and DNS"),
    ("daemon", "Sync cabind"),
    ("library", "Load workspaces and sessions"),
    ("network", "Check internet access"),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartFact {
    label: String,
    value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum PhaseState {
    Pending,
    Running,
    Done,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartPhase {
    id: &'static str,
    title: &'static str,
    state: PhaseState,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartProgress {
    state: &'static str,
    started_at: i64,
    finished_at: Option<i64>,
    error: Option<String>,
    facts: Vec<StartFact>,
    phases: Vec<StartPhase>,
    log_path: Option<String>,
    log_tail: Vec<String>,
    serial_path: Option<String>,
    serial_tail: Vec<String>,
}

struct State {
    phases: Vec<StartPhase>,
    facts: Vec<StartFact>,
    serial: Option<(PathBuf, u64)>,
    error: Option<String>,
    finished_at: Option<i64>,
}

pub struct Attempt {
    started_at: i64,
    started: Instant,
    log: OnceLock<(PathBuf, Log)>,
    state: Mutex<State>,
}

impl Default for Attempt {
    fn default() -> Self {
        Self {
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
            started: Instant::now(),
            log: OnceLock::new(),
            state: Mutex::new(State {
                phases: PHASES
                    .iter()
                    .map(|&(id, title)| StartPhase {
                        id,
                        title,
                        state: PhaseState::Pending,
                        started_at: None,
                        finished_at: None,
                        detail: None,
                    })
                    .collect(),
                facts: Vec::new(),
                serial: None,
                error: None,
                finished_at: None,
            }),
        }
    }
}

impl Attempt {
    pub fn open_log(&self, dir: &Path) -> crate::Result<()> {
        let number = NEXT_LOG.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            "start-{}-{}-{number}.log",
            self.started_at,
            std::process::id()
        ));
        let file = cabin_diagnostics::open_capture(&path, cabin_diagnostics::Capture::New)?;
        let _ = self.log.set((path, Arc::new(Mutex::new(file))));
        self.note("Startup attempt began");
        Ok(())
    }

    pub(crate) fn log(&self) -> Option<&Log> {
        self.log.get().map(|(_, log)| log)
    }

    pub fn log_file(&self) -> Option<File> {
        self.log()?.lock().ok()?.try_clone().ok()
    }

    pub(crate) fn log_path(&self) -> Option<&Path> {
        self.log.get().map(|(path, _)| path.as_path())
    }

    pub fn phase_running(&self, phase: Phase) -> bool {
        self.state.locked().phases[phase as usize].state == PhaseState::Running
    }

    pub fn phase_elapsed(&self, phase: Phase) -> std::time::Duration {
        let state = self.state.locked();
        let phase = &state.phases[phase as usize];
        std::time::Duration::from_millis(phase.started_at.map_or(0, |start| {
            (phase.finished_at.unwrap_or_else(|| self.now()) - start).max(0) as u64
        }))
    }

    pub fn note(&self, detail: &str) {
        if let Some(log) = self.log() {
            log_line(
                log,
                &format!("[+{:.3}s] {detail}", self.started.elapsed().as_secs_f64()),
            );
        }
    }

    fn now(&self) -> i64 {
        self.started_at + self.started.elapsed().as_millis() as i64
    }

    pub fn begin(&self, phase: Phase) {
        self.update(phase, PhaseState::Running, None);
    }

    pub fn complete(&self, phase: Phase, detail: impl Into<String>) {
        self.update(phase, PhaseState::Done, Some(detail.into()));
    }

    pub fn skip(&self, phase: Phase, detail: &str) {
        self.update(phase, PhaseState::Skipped, Some(detail.to_string()));
    }

    /// A completed check can leave the network blocked while the workspace is usable.
    pub fn check_failed(&self, phase: Phase, detail: &str) {
        self.update(phase, PhaseState::Failed, Some(detail.to_string()));
    }

    fn update(&self, phase: Phase, next: PhaseState, detail: Option<String>) {
        let mut state = self.state.locked();
        if state.finished_at.is_some() {
            return;
        }
        let phase = &mut state.phases[phase as usize];
        let now = self.now();
        phase.state = next;
        if next == PhaseState::Running {
            phase.started_at = Some(now);
        } else if next != PhaseState::Skipped {
            phase.finished_at = Some(now);
        }
        self.note(&format!(
            "{}: {next:?}{}",
            phase.title,
            detail
                .as_ref()
                .map(|text| format!(" · {text}"))
                .unwrap_or_default()
        ));
        phase.detail = detail;
    }

    pub fn running(&self) -> bool {
        self.state.locked().finished_at.is_none()
    }

    pub fn finish(&self, error: Option<String>) {
        let mut state = self.state.locked();
        if state.finished_at.is_some() {
            return;
        }
        let now = self.now();
        if let Some(error) = &error {
            if let Some(phase) = state
                .phases
                .iter_mut()
                .find(|p| p.state == PhaseState::Running)
            {
                phase.state = PhaseState::Failed;
                phase.finished_at = Some(now);
                phase.detail = Some(error.clone());
            }
            self.note(&format!("Startup failed: {error}"));
        } else {
            self.note("Startup complete");
        }
        state.finished_at = Some(now);
        state.error = error;
    }

    pub(crate) fn add_fact(&self, label: &str, value: String) {
        let mut state = self.state.locked();
        if let Some(fact) = state.facts.iter_mut().find(|fact| fact.label == label) {
            fact.value = value;
        } else {
            state.facts.push(StartFact {
                label: label.into(),
                value,
            });
        }
    }

    pub(crate) fn machine_facts(&self, cfg: &Config) {
        if let Ok(qemu) = crate::vm::resolve_qemu(cfg) {
            self.add_fact("QEMU", qemu.qemu_bin);
            self.add_fact("UEFI firmware", qemu.firmware_code.display().to_string());
        }
        if cfg!(target_os = "macos") {
            self.add_fact("QEMU release", crate::qemu_engine::RELEASE.into());
        }
        self.accelerator_fact();
        self.add_fact(
            "Resources",
            format!(
                "{} vCPU · {} MB memory · {} disk",
                cfg.cpus, cfg.memory_mb, cfg.disk_size
            ),
        );
        self.add_fact(
            "Network",
            format!(
                "{} · SSH on 127.0.0.1:{}",
                cfg.effective_network_mode(),
                cfg.ssh_connect_port()
            ),
        );
        self.add_fact("Machine folder", cfg.instance_dir().display().to_string());
    }

    pub(crate) fn accelerator_fact(&self) {
        let accel = crate::platform::accel();
        self.add_fact(
            "Accelerator",
            format!(
                "{} ({})",
                accel.accel,
                if accel.emulated {
                    "software emulation"
                } else {
                    "hardware virtualization"
                }
            ),
        );
        if let Some(reason) = crate::accel::fallback_reason().filter(|_| cfg!(windows)) {
            self.add_fact("WHPX fallback", format!("{reason:?}"));
        }
    }

    /// A running VM may already have written into the serial capture.
    pub(crate) fn follow_serial(&self, path: PathBuf, running: bool) {
        let offset = if running {
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        self.state.locked().serial = Some((path, offset));
    }

    pub fn snapshot(&self) -> StartProgress {
        let state = self.state.locked();
        let (serial_path, serial_tail) = state
            .serial
            .as_ref()
            .map(|(path, offset)| {
                (
                    Some(path.display().to_string()),
                    tail(&strip_ansi(&read_from(path, *offset))),
                )
            })
            .unwrap_or_default();
        let log_path = self.log_path();
        StartProgress {
            state: if state.error.is_some() {
                "failed"
            } else if state.finished_at.is_some() {
                "succeeded"
            } else {
                "running"
            },
            started_at: self.started_at,
            finished_at: state.finished_at,
            error: state.error.clone(),
            facts: state.facts.clone(),
            phases: state.phases.clone(),
            log_path: log_path.map(|path| path.display().to_string()),
            log_tail: log_path.map(log_tail).unwrap_or_default(),
            serial_path,
            serial_tail,
        }
    }
}

/// Remove terminal escape sequences and carriage returns from the guest console.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

fn tail(text: &str) -> Vec<String> {
    let mut lines: Vec<_> = text
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(TAIL_LINES)
        .map(str::to_string)
        .collect();
    lines.reverse();
    lines
}

pub(crate) fn log_tail(path: &Path) -> Vec<String> {
    tail(&read_from(path, 0))
}

fn read_from(path: &Path, offset: u64) -> String {
    cabin_diagnostics::tail(path, TAIL_BYTES, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_failure_finishes_the_active_phase() {
        let attempt = Attempt::default();
        attempt.begin(Phase::Gateway);
        attempt.finish(Some("Could not open the gateway".into()));
        let report = attempt.snapshot();
        assert_eq!(report.state, "failed");
        assert_eq!(report.phases[0].state, PhaseState::Failed);
        assert_eq!(report.phases[0].finished_at, report.finished_at);
        assert_eq!(report.phases[1].state, PhaseState::Pending);
    }

    #[test]
    fn network_failure_can_finish_startup() {
        let attempt = Attempt::default();
        attempt.begin(Phase::Network);
        attempt.check_failed(Phase::Network, "Internet blocked");
        attempt.finish(None);
        let report = attempt.snapshot();
        assert_eq!(report.state, "succeeded");
        assert_eq!(report.phases.last().unwrap().state, PhaseState::Failed);
    }

    #[test]
    fn console_text_is_cleaned_and_bounded() {
        let raw = "\x1b[0;32m[  OK  ]\x1b[0m Started ssh.service\r\n\x1b]0;title\x07login: ";
        assert_eq!(strip_ansi(raw), "[  OK  ] Started ssh.service\nlogin: ");
        assert_eq!(tail(&"log\n".repeat(100)).len(), TAIL_LINES);
    }
}
