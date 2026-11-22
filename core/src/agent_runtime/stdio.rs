//! A line-oriented child process for the ACP transport. Each child gets its
//! own process group so a stop also reaches MCP servers and tools it started.

use crate::util::LockExt;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{Error, Result};

type LineHandler = Arc<dyn Fn(&str) + Send + Sync>;
type ExitHandler = Arc<dyn Fn(Option<i32>, &str, bool) + Send + Sync>;

pub struct StdioProcess {
    pid: u32,
    stdin: Mutex<Option<ChildStdin>>,
    child: Arc<Mutex<Child>>,
    stopping: Arc<AtomicBool>,
}

pub struct StdioSpec<'a> {
    pub argv: Vec<String>,
    pub cwd: &'a Path,
    pub env: &'a [(String, String)],
}

impl StdioProcess {
    pub fn spawn(
        spec: StdioSpec<'_>,
        on_line: LineHandler,
        on_exit: ExitHandler,
    ) -> Result<Arc<Self>> {
        let (program, args) = spec
            .argv
            .split_first()
            .ok_or_else(|| Error::Internal("empty agent command".into()))?;
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(spec.cwd)
            .envs(spec.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command
            .spawn()
            .map_err(|error| Error::io(format!("start {program}"), error))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Internal("missing agent stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Internal("missing agent stdout".into()))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Internal("missing agent stderr".into()))?;
        let pid = child.id();
        let child = Arc::new(Mutex::new(child));
        let stopping = Arc::new(AtomicBool::new(false));
        let process_stopping = stopping.clone();
        let tail = Arc::new(Mutex::new(String::new()));

        {
            let tail = tail.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(read) = stderr.read(&mut buf) {
                    if read == 0 {
                        break;
                    }
                    let mut text = tail.locked();
                    text.push_str(&String::from_utf8_lossy(&buf[..read]));
                    if text.len() > 8_000 {
                        let mut cut = text.len() - 8_000;
                        while !text.is_char_boundary(cut) {
                            cut += 1;
                        }
                        *text = text[cut..].to_string();
                    }
                }
            });
        }
        {
            let child = child.clone();
            let stopping = stopping.clone();
            let tail = tail.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::with_capacity(64 * 1024, stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    if line.len() > 16 * 1024 * 1024 {
                        on_line(
                            "{\"type\":\"protocol-error\",\"message\":\"Provider emitted a line over 16 MiB\"}",
                        );
                        break;
                    }
                    let text = line.trim_end_matches(['\r', '\n']);
                    if !text.trim().is_empty() {
                        on_line(text);
                    }
                }
                let code = child.locked()
                    .wait()
                    .ok()
                    .and_then(|status| status.code());
                let expected = stopping.load(Ordering::Acquire);
                let stderr = tail.locked()
                    .trim()
                    .to_string();
                on_exit(code, &stderr, expected);
            });
        }
        Ok(Arc::new(Self {
            pid,
            stdin: Mutex::new(Some(stdin)),
            child,
            stopping: process_stopping,
        }))
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn write_value(&self, value: &Value) -> Result<()> {
        let mut line = serde_json::to_string(value)?;
        line.push('\n');
        self.write_raw(&line)
    }

    pub fn write_raw(&self, text: &str) -> Result<()> {
        let mut stdin = self
            .stdin.locked();
        let stream = stdin
            .as_mut()
            .ok_or_else(|| Error::Unavailable("The agent process is closing".into()))?;
        stream
            .write_all(text.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|error| Error::io("write to the agent", error))
    }

    pub fn stop(&self) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stdin.locked()
            .take();
        let deadline = Instant::now() + Duration::from_millis(700);
        while Instant::now() < deadline {
            if self
                .child.locked()
                .try_wait()
                .ok()
                .flatten()
                .is_some()
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        signal_group(self.pid, libc::SIGTERM);
        let deadline = Instant::now() + Duration::from_millis(900);
        while Instant::now() < deadline {
            if self
                .child.locked()
                .try_wait()
                .ok()
                .flatten()
                .is_some()
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        signal_group(self.pid, libc::SIGKILL);
        let _ = self
            .child.locked()
            .kill();
    }
}

fn signal_group(pid: u32, signal: i32) {
    // SAFETY: `kill` only receives an integer process-group id and signal.
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}
