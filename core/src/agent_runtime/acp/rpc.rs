//! JSON-RPC 2.0 over an agent's stdio, the ACP transport. Notifications are
//! handled in order on the reader thread; requests from the agent each get a
//! thread of their own because some (permission, elicitation) wait for the user.

use crate::util::LockExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};

use super::super::stdio::{StdioProcess, StdioSpec};
use crate::error::{Error, Result};

/// A JSON-RPC error object, as sent or received.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(-32601, format!("Method not found: {method}"))
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(-32603, message)
    }

    fn to_value(&self) -> Value {
        let mut error = json!({"code": self.code, "message": self.message});
        if let Some(data) = &self.data {
            error["data"] = data.clone();
        }
        error
    }

    fn from_value(value: &Value) -> Self {
        Self {
            code: value["code"].as_i64().unwrap_or(-32603),
            message: value["message"]
                .as_str()
                .unwrap_or("The agent reported an error")
                .to_string(),
            data: value.get("data").cloned(),
        }
    }
}

impl From<RpcError> for Error {
    fn from(error: RpcError) -> Self {
        // `data.details` often carries the useful part of an agent's error.
        let detail = error
            .data
            .as_ref()
            .and_then(|data| data["details"].as_str().or_else(|| data.as_str()))
            .map(str::trim)
            .filter(|detail| !detail.is_empty() && *detail != error.message);
        Error::Unavailable(match detail {
            Some(detail) => format!("{} ({detail})", error.message),
            None => error.message,
        })
    }
}

/// What the client side does with the agent's messages.
pub trait Handler: Send + Sync {
    fn notification(&self, method: &str, params: &Value);
    /// Answer one request from the agent. Runs on its own thread; may block.
    fn request(&self, method: &str, params: &Value) -> std::result::Result<Value, RpcError>;
    /// The process ended; `expected` when `Peer::stop` caused it.
    fn exited(&self, code: Option<i32>, stderr: &str, expected: bool);
}

type Reply = std::result::Result<Value, RpcError>;

pub struct Peer {
    process: OnceLock<Arc<StdioProcess>>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<String, mpsc::Sender<Reply>>>,
    closed: AtomicBool,
}

impl Peer {
    pub fn spawn(spec: StdioSpec<'_>, handler: Arc<dyn Handler>) -> Result<Arc<Self>> {
        let peer = Arc::new(Self {
            process: OnceLock::new(),
            next_id: AtomicI64::new(1),
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });
        let reader = Arc::downgrade(&peer);
        let line_handler = handler.clone();
        let exit = Arc::downgrade(&peer);
        let process = StdioProcess::spawn(
            spec,
            Arc::new(move |line: &str| {
                if let Some(peer) = reader.upgrade() {
                    peer.receive(line, &line_handler);
                }
            }),
            Arc::new(move |code, stderr: &str, expected| {
                if let Some(peer) = exit.upgrade() {
                    peer.close(stderr);
                }
                handler.exited(code, stderr, expected);
            }),
        )?;
        let _ = peer.process.set(process);
        Ok(peer)
    }

    pub fn pid(&self) -> u32 {
        self.process.get().map_or(0, |process| process.pid())
    }

    fn process(&self) -> Result<&Arc<StdioProcess>> {
        self.process
            .get()
            .ok_or_else(|| Error::Unavailable("The agent is not running".into()))
    }

    /// Send a request and wait for its result; `None` waits as long as the
    /// process lives (a prompt turn can run for hours).
    pub fn request(&self, method: &str, params: Value, timeout: Option<Duration>) -> Result<Value> {
        self.request_raw(method, params, timeout)?
            .map_err(Error::from)
    }

    /// Like `request`, but an error answer stays an `RpcError` for the caller to inspect.
    pub fn request_raw(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Reply> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::Unavailable("The agent has stopped".into()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.locked()
            .insert(id.to_string(), tx);
        let sent = self.process().and_then(|process| {
            process.write_value(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }))
        });
        if let Err(error) = sent {
            self.forget(id);
            return Err(error);
        }
        let reply = match timeout {
            Some(timeout) => rx.recv_timeout(timeout).map_err(|error| match error {
                RecvTimeoutError::Timeout => {
                    self.forget(id);
                    Error::Unavailable(format!("The agent did not answer {method} in time"))
                }
                RecvTimeoutError::Disconnected => {
                    Error::Unavailable("The agent stopped before it answered".into())
                }
            })?,
            None => rx
                .recv()
                .map_err(|_| Error::Unavailable("The agent stopped before it answered".into()))?,
        };
        Ok(reply)
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.process()?.write_value(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    pub fn stop(&self) {
        if let Some(process) = self.process.get() {
            process.stop();
        }
    }

    fn forget(&self, id: i64) {
        self.pending.locked()
            .remove(&id.to_string());
    }

    fn close(&self, stderr: &str) {
        self.closed.store(true, Ordering::Release);
        let pending: Vec<_> = self
            .pending.locked()
            .drain()
            .collect();
        let message = last_line(stderr)
            .map(|line| format!("The agent stopped: {line}"))
            .unwrap_or_else(|| "The agent stopped".into());
        for (_, tx) in pending {
            let _ = tx.send(Err(RpcError::internal(message.clone())));
        }
    }

    fn receive(self: &Arc<Self>, line: &str, handler: &Arc<dyn Handler>) {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            // Agents log to stdout now and then; only JSON lines are protocol.
            return;
        };
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").filter(|id| !id.is_null()).cloned();
        match (method, id) {
            (Some(method), Some(id)) => {
                let peer = self.clone();
                let handler = handler.clone();
                let method = method.to_string();
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                std::thread::spawn(move || {
                    let reply = handler.request(&method, &params);
                    let mut response = json!({"jsonrpc": "2.0", "id": id});
                    match reply {
                        Ok(result) => response["result"] = result,
                        Err(error) => response["error"] = error.to_value(),
                    }
                    if let Ok(process) = peer.process() {
                        let _ = process.write_value(&response);
                    }
                });
            }
            (Some(method), None) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                handler.notification(method, &params);
            }
            (None, Some(id)) => {
                let key = match &id {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                let tx = self
                    .pending.locked()
                    .remove(&key);
                if let Some(tx) = tx {
                    let reply = match message.get("error") {
                        Some(error) if !error.is_null() => Err(RpcError::from_value(error)),
                        _ => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(reply);
                }
            }
            (None, None) => {}
        }
    }
}

fn last_line(text: &str) -> Option<&str> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
}
