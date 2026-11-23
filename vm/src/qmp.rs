//! Minimal QMP and QEMU guest-agent clients over AF_UNIX sockets.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::Result;
use crate::sockets::UnixStream;
use crate::util::message;

pub struct QmpClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl QmpClient {
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self> {
        let stream = UnixStream::connect(path).map_err(|error| {
            crate::Error::Message(format!(
                "cannot connect to QMP socket {}: {error}",
                path.display()
            ))
        })?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let reader = BufReader::new(stream.try_clone()?);
        let mut client = Self {
            stream,
            reader,
            next_id: 1,
        };
        let greeting = client.read_message()?;
        if greeting.get("QMP").is_none() {
            return message(format!("unexpected QMP greeting from {}", path.display()));
        }
        client.execute("qmp_capabilities", None)?;
        Ok(client)
    }

    fn read_message(&mut self) -> Result<Value> {
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line)?;
            if read == 0 {
                return message("QMP connection closed unexpectedly");
            }
            let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if message.get("event").is_some() {
                continue;
            }
            return Ok(message);
        }
    }

    pub fn execute(&mut self, command: &str, arguments: Option<Value>) -> Result<Value> {
        let request_id = self.next_id;
        self.next_id += 1;
        let mut payload = json!({"execute": command, "id": request_id});
        if let Some(arguments) = arguments {
            payload["arguments"] = arguments;
        }
        self.stream.write_all(payload.to_string().as_bytes())?;
        self.stream.write_all(b"\n")?;
        self.stream.flush()?;
        loop {
            let response = self.read_message()?;
            if response.get("id").and_then(Value::as_u64) != Some(request_id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                let description = error
                    .get("desc")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| error.to_string());
                return message(format!("QMP {command} failed: {description}"));
            }
            return Ok(response.get("return").cloned().unwrap_or(Value::Null));
        }
    }
}

impl Drop for QmpClient {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

pub struct QgaClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl QgaClient {
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self> {
        let stream = UnixStream::connect(path).map_err(|error| {
            crate::Error::Message(format!(
                "cannot connect to guest-agent socket {}: {error}",
                path.display()
            ))
        })?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self { stream, reader })
    }

    pub fn execute(&mut self, command: &str, arguments: Option<Value>) -> Result<Value> {
        let mut payload = json!({"execute": command});
        if let Some(arguments) = arguments {
            payload["arguments"] = arguments;
        }
        self.stream.write_all(payload.to_string().as_bytes())?;
        self.stream.write_all(b"\n")?;
        self.stream.flush()?;
        let mut line = String::new();
        let read = self.reader.read_line(&mut line)?;
        if read == 0 {
            return message("guest-agent connection closed unexpectedly");
        }
        let response: Value = serde_json::from_str(line.trim())?;
        if let Some(error) = response.get("error") {
            let description = error
                .get("desc")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| error.to_string());
            return message(format!("guest-agent {command} failed: {description}"));
        }
        Ok(response.get("return").cloned().unwrap_or(Value::Null))
    }
}

impl Drop for QgaClient {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

pub fn qmp_query_status(path: &Path, timeout: Duration) -> Option<Value> {
    let mut client = QmpClient::connect(path, timeout).ok()?;
    client.execute("query-status", None).ok()
}
