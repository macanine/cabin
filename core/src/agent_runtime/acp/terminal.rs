//! Client-side terminals (`terminal/*`): commands the agent asks us to run in
//! the workspace. Output is kept up to the agent's byte limit, trimmed from the
//! front on a character boundary, and each change is reported so a tool card
//! embedding the terminal can show it live.

use crate::util::LockExt;
use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::rpc::RpcError;

/// Without a limit from the agent, keep this much output.
const DEFAULT_LIMIT: usize = 1024 * 1024;

#[derive(Default)]
struct Output {
    text: String,
    truncated: bool,
    exit: Option<(Option<i32>, Option<String>)>,
}

struct Terminal {
    child: Mutex<Option<Child>>,
    limit: usize,
    output: Mutex<Output>,
    done: Condvar,
}

impl Terminal {
    fn push(&self, chunk: &str) {
        let mut output = self.output.locked();
        output.text.push_str(chunk);
        if output.text.len() > self.limit {
            let mut cut = output.text.len() - self.limit;
            while !output.text.is_char_boundary(cut) {
                cut += 1;
            }
            output.text.drain(..cut);
            output.truncated = true;
        }
    }

    fn snapshot(&self) -> Value {
        let output = self.output.locked();
        let mut value = json!({"output": output.text, "truncated": output.truncated});
        if let Some((code, signal)) = &output.exit {
            value["exitStatus"] = json!({"exitCode": code, "signal": signal});
        }
        value
    }

    fn kill(&self) {
        if let Some(child) = self.child.locked().as_mut() {
            kill_group(child);
        }
    }

    fn abort_start(&self) {
        if let Some(mut child) = self.child.locked().take() {
            kill_group(&mut child);
            let _ = child.wait();
        }
    }

    fn record_exit(&self) {
        let exit = loop {
            {
                let mut slot = self.child.locked();
                let Some(child) = slot.as_mut() else { return };
                match child.try_wait() {
                    Ok(Some(status)) => {
                        slot.take();
                        break (
                            status.code(),
                            status.signal().map(|signal| format!("signal {signal}")),
                        );
                    }
                    Ok(None) => {}
                    Err(_) => {
                        slot.take();
                        break (None, None);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        self.output.locked().exit = Some(exit);
        self.done.notify_all();
    }
}

fn kill_group(child: &mut Child) {
    // SAFETY: signals the process group this terminal created.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
}

type OnOutput = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct TerminalSet {
    open: HashMap<String, Arc<Terminal>>,
    closed: bool,
}

pub struct Terminals {
    next: AtomicU64,
    state: Mutex<TerminalSet>,
    on_output: OnOutput,
}

impl Terminals {
    pub fn new(on_output: OnOutput) -> Self {
        Self {
            next: AtomicU64::new(1),
            state: Mutex::default(),
            on_output,
        }
    }

    fn get(&self, params: &Value) -> Result<Arc<Terminal>, RpcError> {
        let id = params["terminalId"].as_str().unwrap_or_default();
        self.state
            .locked()
            .open
            .get(id)
            .cloned()
            .ok_or_else(|| RpcError::invalid_params(format!("Unknown terminal {id}")))
    }

    /// The text a tool card shows for an embedded terminal.
    pub fn text(&self, id: &str) -> Option<(String, Option<i32>)> {
        let terminal = self.state.locked().open.get(id).cloned()?;
        let output = terminal.output.locked();
        Some((
            output.text.clone(),
            output.exit.as_ref().and_then(|exit| exit.0),
        ))
    }

    pub fn create(
        &self,
        params: &Value,
        root: &Path,
        env: &[(String, String)],
    ) -> Result<Value, RpcError> {
        let command = params["command"]
            .as_str()
            .filter(|command| !command.trim().is_empty())
            .ok_or_else(|| RpcError::invalid_params("terminal/create needs a command"))?;
        let args: Vec<String> = params["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|arg| arg.as_str().map(str::to_string))
            .collect();
        let cwd = params["cwd"]
            .as_str()
            .map(Path::new)
            .filter(|cwd| cwd.is_absolute())
            .unwrap_or(root);
        let limit = match params["outputByteLimit"].as_u64() {
            Some(limit) => usize::try_from(limit)
                .map_err(|_| RpcError::invalid_params("terminal output limit is too large"))?,
            None => DEFAULT_LIMIT,
        };
        // A bare command line with no args runs through the shell, as agents expect.
        let mut process = if args.is_empty() && command.contains(char::is_whitespace) {
            let mut process = Command::new("/bin/sh");
            process.arg("-c").arg(command);
            process
        } else {
            let mut process = Command::new(command);
            process.args(&args);
            process
        };
        process
            .current_dir(cwd)
            .envs(env.iter().map(|(key, value)| (key, value)))
            .envs(
                params["env"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|var| {
                        Some((
                            var["name"].as_str()?.to_string(),
                            var["value"].as_str()?.to_string(),
                        ))
                    }),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut state = self.state.locked();
        if state.closed {
            return Err(RpcError::internal("The agent is stopping"));
        }
        let mut child = process
            .spawn()
            .map_err(|error| RpcError::internal(format!("Could not run {command}: {error}")))?;
        let id = format!("term-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let streams = [
            child
                .stdout
                .take()
                .map(|out| Box::new(out) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|err| Box::new(err) as Box<dyn Read + Send>),
        ];
        let terminal = Arc::new(Terminal {
            child: Mutex::new(Some(child)),
            limit,
            output: Mutex::new(Output::default()),
            done: Condvar::new(),
        });
        let readers = streams
            .into_iter()
            .flatten()
            .map(|mut stream| {
                let terminal = terminal.clone();
                let on_output = self.on_output.clone();
                let id = id.clone();
                std::thread::Builder::new().spawn(move || {
                    let mut buf = [0u8; 8192];
                    let mut carry = Vec::new();
                    while let Ok(read) = stream.read(&mut buf) {
                        if read == 0 {
                            break;
                        }
                        carry.extend_from_slice(&buf[..read]);
                        let valid = match std::str::from_utf8(&carry) {
                            Ok(_) => carry.len(),
                            Err(error) if error.error_len().is_none() => error.valid_up_to(),
                            Err(_) => carry.len(),
                        };
                        let text = String::from_utf8_lossy(&carry[..valid]).into_owned();
                        carry.drain(..valid);
                        terminal.push(&text);
                        on_output(&id);
                    }
                    if !carry.is_empty() {
                        terminal.push(&String::from_utf8_lossy(&carry));
                        on_output(&id);
                    }
                })
            })
            .collect::<std::io::Result<Vec<_>>>();
        let workers = readers.and_then(|readers| {
            let worker = terminal.clone();
            let on_output = self.on_output.clone();
            let id = id.clone();
            std::thread::Builder::new().spawn(move || {
                for reader in readers {
                    let _ = reader.join();
                }
                worker.record_exit();
                on_output(&id);
            })
        });
        if let Err(error) = workers {
            terminal.abort_start();
            return Err(RpcError::internal(format!(
                "Could not start terminal workers: {error}"
            )));
        }
        state.open.insert(id.clone(), terminal);
        Ok(json!({"terminalId": id}))
    }

    pub fn output(&self, params: &Value) -> Result<Value, RpcError> {
        Ok(self.get(params)?.snapshot())
    }

    pub fn wait_for_exit(&self, params: &Value) -> Result<Value, RpcError> {
        let terminal = self.get(params)?;
        let mut output = terminal.output.locked();
        loop {
            if let Some((code, signal)) = &output.exit {
                return Ok(json!({"exitCode": code, "signal": signal}));
            }
            output = terminal
                .done
                .wait(output)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub fn kill(&self, params: &Value) -> Result<Value, RpcError> {
        self.get(params)?.kill();
        Ok(json!({}))
    }

    /// Kill the command if it still runs. Keep output for embedding tool cards.
    pub fn release(&self, params: &Value) -> Result<Value, RpcError> {
        self.kill(params)
    }

    pub fn kill_all(&self) {
        let mut state = self.state.locked();
        state.closed = true;
        for terminal in state.open.values() {
            terminal.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_a_command_and_keeps_only_the_tail_within_the_limit() {
        let terminals = Terminals::new(Arc::new(|_| {}));
        let dir = tempfile::tempdir().unwrap();
        let created = terminals
            .create(
                &json!({"command": "sh", "args": ["-c", "printf 'abcdefghij'; exit 3"], "outputByteLimit": 4}),
                dir.path(),
                &[],
            )
            .unwrap();
        let exit = terminals.wait_for_exit(&created).unwrap();
        assert_eq!(exit["exitCode"], 3);
        let output = terminals.output(&created).unwrap();
        assert_eq!(output["output"], "ghij");
        assert_eq!(output["truncated"], true);
        assert_eq!(output["exitStatus"]["exitCode"], 3);
    }

    #[test]
    fn a_bare_command_line_runs_through_the_shell_and_release_kills_it() {
        let terminals = Terminals::new(Arc::new(|_| {}));
        let dir = tempfile::tempdir().unwrap();
        let created = terminals
            .create(&json!({"command": "sleep 30"}), dir.path(), &[])
            .unwrap();
        terminals.release(&created).unwrap();
        let exit = terminals.wait_for_exit(&created).unwrap();
        assert!(exit["exitCode"].is_null());
    }

    #[test]
    fn shutdown_rejects_late_terminal_creation() {
        let terminals = Terminals::new(Arc::new(|_| {}));
        terminals.kill_all();
        assert!(
            terminals
                .create(&json!({"command": "true"}), Path::new("/"), &[])
                .is_err()
        );
    }
}
