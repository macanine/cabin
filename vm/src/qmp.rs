//! Minimal QMP and QEMU guest-agent clients over AF_UNIX sockets.

use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::Result;
use crate::sockets::{self, UnixStream};
use crate::util::message;

struct Wire {
    reader: BufReader<UnixStream>,
    timeout: Duration,
    service: &'static str,
}

impl Wire {
    fn connect(
        path: &Path,
        deadline: Instant,
        timeout: Duration,
        service: &'static str,
    ) -> Result<Self> {
        sockets::validate_path(path)?;
        let stream = sockets::connect(path, remaining(deadline)?).map_err(|error| {
            crate::Error::Message(format!(
                "cannot connect to {service} socket {}: {error}",
                path.display()
            ))
        })?;
        Ok(Self {
            reader: BufReader::new(stream),
            timeout,
            service,
        })
    }

    fn read_message(&mut self, deadline: Instant) -> Result<Value> {
        let mut line = Vec::new();
        loop {
            // A peer sending partial lines or unsolicited events cannot renew
            // the deadline. Cap unterminated messages to bound memory as well.
            self.reader
                .get_ref()
                .set_read_timeout(Some(remaining(deadline)?))?;
            let chunk = self.reader.fill_buf()?;
            if chunk.is_empty() {
                return message(format!("{} connection closed unexpectedly", self.service));
            }
            let end = chunk.iter().position(|&byte| byte == b'\n');
            let length = end.map_or(chunk.len(), |index| index + 1);
            if line.len() + length > 8 * 1024 * 1024 {
                return message(format!("{} message exceeded 8 MiB", self.service));
            }
            line.extend_from_slice(&chunk[..length]);
            self.reader.consume(length);
            if end.is_some() {
                return Ok(serde_json::from_slice(&line)?);
            }
        }
    }

    fn send(&mut self, payload: Value, deadline: Instant) -> Result<()> {
        let mut request = serde_json::to_vec(&payload)?;
        request.push(b'\n');
        let mut pending = request.as_slice();
        let stream = self.reader.get_mut();
        while !pending.is_empty() {
            stream.set_write_timeout(Some(remaining(deadline)?))?;
            match stream.write(pending) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(written) => pending = &pending[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn result(&self, command: &str, response: &Value) -> Result<Value> {
        if let Some(error) = response.get("error") {
            let detail = error
                .get("desc")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string());
            return message(format!("{} {command} failed: {detail}", self.service));
        }
        Ok(response.get("return").cloned().unwrap_or(Value::Null))
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|wait| !wait.is_zero())
        .map(|wait| wait.max(Duration::from_millis(1)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "QEMU control request timed out"))
}

impl Drop for Wire {
    fn drop(&mut self) {
        let _ = self.reader.get_ref().shutdown(std::net::Shutdown::Both);
    }
}

pub struct QmpClient {
    wire: Wire,
    next_id: u64,
}

impl QmpClient {
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self> {
        let deadline = Instant::now() + timeout;
        let wire = Wire::connect(path, deadline, timeout, "QMP")?;
        let mut client = Self { wire, next_id: 1 };
        if client.wire.read_message(deadline)?.get("QMP").is_none() {
            return message(format!("unexpected QMP greeting from {}", path.display()));
        }
        client.execute_until("qmp_capabilities", None, deadline)?;
        Ok(client)
    }

    pub fn execute(&mut self, command: &str, arguments: Option<Value>) -> Result<Value> {
        self.execute_until(command, arguments, Instant::now() + self.wire.timeout)
    }

    pub(crate) fn execute_until(
        &mut self,
        command: &str,
        arguments: Option<Value>,
        deadline: Instant,
    ) -> Result<Value> {
        let request_id = self.next_id;
        self.next_id += 1;
        let mut payload = json!({"execute": command, "id": request_id});
        if let Some(arguments) = arguments {
            payload["arguments"] = arguments;
        }
        self.wire.send(payload, deadline)?;
        loop {
            let response = self.wire.read_message(deadline)?;
            if response.get("id").and_then(Value::as_u64) == Some(request_id) {
                return self.wire.result(command, &response);
            }
        }
    }
}

pub struct QgaClient(Wire);

impl QgaClient {
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self> {
        Ok(Self(Wire::connect(
            path,
            Instant::now() + timeout,
            timeout,
            "guest-agent",
        )?))
    }

    pub fn execute(&mut self, command: &str, arguments: Option<Value>) -> Result<Value> {
        let deadline = Instant::now() + self.0.timeout;
        let mut payload = json!({"execute": command});
        if let Some(arguments) = arguments {
            payload["arguments"] = arguments;
        }
        self.0.send(payload, deadline)?;
        let response = self.0.read_message(deadline)?;
        self.0.result(command, &response)
    }
}

pub fn qmp_query_status(path: &Path, timeout: Duration) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    let mut client = QmpClient::connect(path, timeout).ok()?;
    client.execute_until("query-status", None, deadline).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sockets::UnixListener;

    #[test]
    fn qmp_events_cannot_extend_the_command_deadline() {
        let dir = sockets::test_dir();
        let path = dir.join("qmp.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(b"{\"QMP\":{}}\n{\"return\":{},\"id\":1}\n")
                .unwrap();
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until && stream.write_all(b"{\"event\":\"STOP\"}\n").is_ok() {
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let mut client = QmpClient::connect(&path, Duration::from_millis(200)).unwrap();
        let start = Instant::now();
        assert!(client.execute("query-status", None).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        drop(client);
        server.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn qga_partial_reply_cannot_extend_the_command_deadline() {
        let dir = sockets::test_dir();
        let path = dir.join("qga.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until && stream.write_all(b" ").is_ok() {
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let mut client = QgaClient::connect(&path, Duration::from_millis(200)).unwrap();
        let start = Instant::now();
        assert!(client.execute("guest-ping", None).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        drop(client);
        server.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
