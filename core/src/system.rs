//! External command execution behind a trait so tests can fake the OS.
//!
//! Commands are always argv arrays; nothing here goes through a shell.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct CommandSpec {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<String>,
    pub timeout: Option<Duration>,
    /// Maximum bytes to capture from each output pipe, when set.
    pub output_limit: Option<u64>,
}

impl CommandSpec {
    pub fn new<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    pub fn env(mut self, vars: impl IntoIterator<Item = (String, String)>) -> Self {
        self.env.extend(vars);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn output_limit(mut self, bytes: u64) -> Self {
        self.output_limit = Some(bytes);
        self
    }

    pub fn display(&self) -> String {
        self.argv.join(" ")
    }
}

fn running_as_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Uid:"))
                .and_then(|ids| ids.split_whitespace().next().map(|uid| uid == "0"))
        })
        .unwrap_or(false)
}

/// A command that needs root. Inside the VM the user has passwordless sudo
/// and there is nobody to answer a polkit dialog, so `sudo -n` is used (it
/// fails fast instead of prompting). `CABIN_PRIVILEGE=pkexec` switches to
/// polkit for desktop installs.
pub fn privileged<I, S>(argv: I) -> CommandSpec
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let argv: Vec<String> = argv.into_iter().map(Into::into).collect();
    if running_as_root() {
        return CommandSpec::new(argv);
    }
    let prefix: Vec<String> = match std::env::var("CABIN_PRIVILEGE").as_deref() {
        Ok("pkexec") => vec!["pkexec".into()],
        _ => vec!["sudo".into(), "-n".into()],
    };
    CommandSpec::new(prefix.into_iter().chain(argv))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn combined(&self) -> String {
        let mut text = self.stdout.trim_end().to_string();
        if !self.stderr.trim().is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(self.stderr.trim_end());
        }
        text
    }
}

pub trait System: Send + Sync {
    fn run(&self, spec: &CommandSpec) -> Result<Output>;

    /// Run and require exit code 0.
    fn check(&self, spec: &CommandSpec) -> Result<String> {
        let output = self.run(spec)?;
        if output.success() {
            Ok(output.stdout)
        } else {
            Err(Error::Command {
                message: format!(
                    "`{}` failed{}",
                    spec.display(),
                    output
                        .code
                        .map(|code| format!(" with exit code {code}"))
                        .unwrap_or_default()
                ),
                output: output.combined(),
            })
        }
    }
}

/// Runs real processes with an optional timeout.
pub struct HostSystem {
    pub path: String,
}

impl System for HostSystem {
    fn run(&self, spec: &CommandSpec) -> Result<Output> {
        let (program, args) = spec
            .argv
            .split_first()
            .ok_or_else(|| Error::invalid("empty command"))?;
        let mut command = Command::new(program);
        command
            .args(args)
            .env("PATH", &self.path)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        if spec.timeout.is_some() {
            command.process_group(0);
        }
        let started = Instant::now();
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::Unavailable(format!("{program} is not installed"))
            } else {
                Error::io(format!("start {program}"), error)
            }
        })?;
        let stdout = capture(child.stdout.take(), spec.output_limit);
        let stderr = capture(child.stderr.take(), spec.output_limit);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if spec.timeout.is_some_and(|limit| started.elapsed() >= limit) {
                return Err(command_timeout(spec, &mut child));
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let mut receive = |pipe: Receiver<Option<String>>| {
            let captured = match spec.timeout {
                Some(limit) => match pipe.recv_timeout(limit.saturating_sub(started.elapsed())) {
                    Ok(captured) => captured,
                    Err(RecvTimeoutError::Disconnected) => Some(String::new()),
                    Err(RecvTimeoutError::Timeout) => {
                        return Err(command_timeout(spec, &mut child));
                    }
                },
                None => pipe.recv().unwrap_or_else(|_| Some(String::new())),
            };
            captured.ok_or_else(|| Error::Command {
                message: format!("`{}` exceeded its output limit", spec.display()),
                output: String::new(),
            })
        };
        Ok(Output {
            code: status.code(),
            stdout: receive(stdout)?,
            stderr: receive(stderr)?,
        })
    }
}

fn capture(
    pipe: Option<impl Read + Send + 'static>,
    limit: Option<u64>,
) -> Receiver<Option<String>> {
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            if let Some(limit) = limit {
                let _ = pipe.take(limit.saturating_add(1)).read_to_end(&mut bytes);
                if bytes.len() as u64 > limit {
                    let _ = send.send(None);
                    return;
                }
            } else {
                let _ = pipe.read_to_end(&mut bytes);
            }
        }
        let _ = send.send(Some(String::from_utf8(bytes).unwrap_or_default()));
    });
    receive
}

fn command_timeout(spec: &CommandSpec, child: &mut Child) -> Error {
    // A child that already exited can still have descendants holding its pipes.
    // Timed commands run in a separate group so the deadline reaches those too.
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    let _ = child.kill();
    let _ = child.wait();
    Error::Command {
        message: format!("`{}` timed out", spec.display()),
        output: String::new(),
    }
}

#[cfg(test)]
pub mod fake {
    //! Scripted `System` for tests.
    use super::*;
    use std::sync::Mutex;

    type Handler = Box<dyn Fn(&CommandSpec) -> Option<Result<Output>> + Send + Sync>;

    #[derive(Default)]
    pub struct FakeSystem {
        pub calls: Mutex<Vec<String>>,
        handlers: Mutex<Vec<Handler>>,
    }

    pub fn ok(stdout: &str) -> Result<Output> {
        Ok(Output {
            code: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        })
    }

    pub fn fail(stderr: &str) -> Result<Output> {
        Ok(Output {
            code: Some(1),
            stdout: String::new(),
            stderr: stderr.into(),
        })
    }

    impl FakeSystem {
        pub fn on(
            &self,
            handler: impl Fn(&CommandSpec) -> Option<Result<Output>> + Send + Sync + 'static,
        ) {
            self.handlers.lock().unwrap().push(Box::new(handler));
        }

        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl System for FakeSystem {
        fn run(&self, spec: &CommandSpec) -> Result<Output> {
            self.calls.lock().unwrap().push(spec.display());
            for handler in self.handlers.lock().unwrap().iter().rev() {
                if let Some(result) = handler(spec) {
                    return result;
                }
            }
            ok("")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_system_captures_output_and_exit_code() -> Result<()> {
        let system = HostSystem {
            path: "/usr/bin:/bin".into(),
        };
        let output = system.run(&CommandSpec::new([
            "sh",
            "-c",
            "echo out; echo err >&2; exit 3",
        ]))?;
        assert_eq!(output.code, Some(3));
        assert_eq!(output.stdout.trim(), "out");
        assert_eq!(output.stderr.trim(), "err");
        assert!(system.check(&CommandSpec::new(["false"])).is_err());
        Ok(())
    }

    #[test]
    fn deadline_also_bounds_pipes_inherited_by_a_background_child() {
        let system = HostSystem {
            path: "/usr/bin:/bin".into(),
        };
        let started = Instant::now();
        let result = system.run(
            &CommandSpec::new(["sh", "-c", "sleep 5 & exit 0"]).timeout(Duration::from_millis(100)),
        );
        assert!(matches!(result, Err(Error::Command { .. })));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn capped_capture_rejects_overlong_output() {
        let system = HostSystem {
            path: "/usr/bin:/bin".into(),
        };
        let result = system.run(
            &CommandSpec::new(["sh", "-c", "printf 123456789"])
                .timeout(Duration::from_secs(2))
                .output_limit(8),
        );
        assert!(
            matches!(result, Err(Error::Command { message, .. }) if message.contains("output limit"))
        );
    }

    #[test]
    fn host_system_times_out() {
        let system = HostSystem {
            path: "/usr/bin:/bin".into(),
        };
        let result =
            system.run(&CommandSpec::new(["sleep", "5"]).timeout(Duration::from_millis(100)));
        assert!(matches!(result, Err(Error::Command { .. })));
    }
}
