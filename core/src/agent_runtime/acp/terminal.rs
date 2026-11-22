//! Client-side terminals (`terminal/*`): commands the agent asks us to run in
//! the workspace. Output is kept up to the agent's byte limit, trimmed from the
//! front on a character boundary, and each change is reported so a tool card
//! embedding the terminal can show it live.

use crate::util::LockExt;
use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

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
    pid: u32,
    limit: usize,
    output: Mutex<Output>,
    done: Condvar,
}

impl Terminal {
    fn push(&self, chunk: &str) {
        let mut output = self
            .output.locked();
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
        let output = self
            .output.locked();
        let mut value = json!({"output": output.text, "truncated": output.truncated});
        if let Some((code, signal)) = &output.exit {
            value["exitStatus"] = json!({"exitCode": code, "signal": signal});
        }
        value
    }

    fn kill(&self) {
        // SAFETY: signals the process group this terminal created.
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGKILL);
        }
    }
}

type OnOutput = Arc<dyn Fn(&str) + Send + Sync>;

pub struct Terminals {
    next: AtomicU64,
    open: Mutex<HashMap<String, Arc<Terminal>>>,
    on_output: OnOutput,
}

impl Terminals {
    pub fn new(on_output: OnOutput) -> Self {
        Self {
            next: AtomicU64::new(1),
            open: Mutex::new(HashMap::new()),
            on_output,
        }
    }

    fn get(&self, params: &Value) -> Result<Arc<Terminal>, RpcError> {
        let id = params["terminalId"].as_str().unwrap_or_default();
        self.open.locked()
            .get(id)
            .cloned()
            .ok_or_else(|| RpcError::invalid_params(format!("Unknown terminal {id}")))
    }

    /// The text a tool card shows for an embedded terminal.
    pub fn text(&self, id: &str) -> Option<(String, Option<i32>)> {
        let terminal = self
            .open.locked()
            .get(id)
            .cloned()?;
        let output = terminal
            .output.locked();
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
        let limit = params["outputByteLimit"]
            .as_u64()
            .map_or(DEFAULT_LIMIT, |limit| limit as usize);
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
            pid: child.id(),
            child: Mutex::new(Some(child)),
            limit,
            output: Mutex::new(Output::default()),
            done: Condvar::new(),
        });
        let readers: Vec<_> = streams
            .into_iter()
            .flatten()
            .map(|mut stream| {
                let terminal = terminal.clone();
                let on_output = self.on_output.clone();
                let id = id.clone();
                std::thread::spawn(move || {
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
                })
            })
            .collect();
        {
            let terminal = terminal.clone();
            let on_output = self.on_output.clone();
            let id = id.clone();
            std::thread::spawn(move || {
                for reader in readers {
                    let _ = reader.join();
                }
                let child = terminal
                    .child.locked()
                    .take();
                let status = child.and_then(|mut child| child.wait().ok());
                let exit = status.map_or((None, Some("SIGKILL".into())), |status| {
                    use std::os::unix::process::ExitStatusExt;
                    (
                        status.code(),
                        status.signal().map(|signal| format!("signal {signal}")),
                    )
                });
                terminal
                    .output.locked()
                    .exit = Some(exit);
                terminal.done.notify_all();
                on_output(&id);
            });
        }
        self.open.locked()
            .insert(id.clone(), terminal);
        Ok(json!({"terminalId": id}))
    }

    pub fn output(&self, params: &Value) -> Result<Value, RpcError> {
        Ok(self.get(params)?.snapshot())
    }

    pub fn wait_for_exit(&self, params: &Value) -> Result<Value, RpcError> {
        let terminal = self.get(params)?;
        let mut output = terminal
            .output.locked();
        while output.exit.is_none() {
            output = terminal
                .done
                .wait(output)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let (code, signal) = output.exit.clone().unwrap_or_default();
        Ok(json!({"exitCode": code, "signal": signal}))
    }

    pub fn kill(&self, params: &Value) -> Result<Value, RpcError> {
        self.get(params)?.kill();
        Ok(json!({}))
    }

    /// Kill the command if it still runs and forget the terminal.
    pub fn release(&self, params: &Value) -> Result<Value, RpcError> {
        let terminal = self.get(params)?;
        if terminal
            .output.locked()
            .exit
            .is_none()
        {
            terminal.kill();
        }
        // Keep the output: a tool card may still embed it.
        Ok(json!({}))
    }

    pub fn kill_all(&self) {
        for terminal in self
            .open.locked()
            .values()
        {
            if terminal
                .output.locked()
                .exit
                .is_none()
            {
                terminal.kill();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
        // Readers may finish just after the exit is recorded on a busy machine.
        std::thread::sleep(Duration::from_millis(50));
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
}
