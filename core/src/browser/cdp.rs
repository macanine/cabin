//! Loopback-only DevTools transport. Events received while waiting for a
//! response are retained so a newly attached target is never lost.
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Error, Result, error};

const MAX_FRAME: u64 = 32 * 1024 * 1024;

pub(super) fn http_json(port: u16, path: &str) -> Result<Value> {
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .timeout(Duration::from_millis(1500))
        .build();
    let response = agent
        .get(&format!("http://127.0.0.1:{port}{path}"))
        .call()
        .map_err(|e| error(format!("DevTools HTTP: {e}")))?;
    let mut body = String::new();
    response
        .into_reader()
        .take(MAX_FRAME)
        .read_to_string(&mut body)?;
    Ok(serde_json::from_str(&body)?)
}

struct WebSocket {
    stream: TcpStream,
}

impl WebSocket {
    fn connect(address: &str, timeout: Duration) -> Result<Self> {
        let url = url::Url::parse(address)?;
        if url.scheme() != "ws"
            || !matches!(
                url.host_str(),
                Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
            )
        {
            return Err(error("DevTools WebSocket must be on loopback"));
        }
        let port = url
            .port()
            .ok_or_else(|| error("DevTools WebSocket has no port"))?;
        // Chrome is started with a loopback listener. Do not resolve a name or
        // consult a proxy even when the reported URL uses localhost.
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let key = STANDARD.encode(Uuid::new_v4().as_bytes());
        let mut path = url.path().to_owned();
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        let host = format!("127.0.0.1:{port}");
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nOrigin: http://{host}\r\n\r\n"
        )?;
        // Read exactly through the headers: Chrome may coalesce its first
        // event with the upgrade response.
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            if header.len() >= 16384 {
                return Err(error("WebSocket handshake header too large"));
            }
            let mut byte = [0];
            stream.read_exact(&mut byte)?;
            header.push(byte[0]);
        }
        let status = header.split(|b| *b == b'\n').next().unwrap_or_default();
        if !status.windows(5).any(|v| v == b" 101 ") {
            return Err(error("WebSocket handshake failed"));
        }
        Ok(Self { stream })
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<()> {
        let random = Uuid::new_v4();
        let mask = &random.as_bytes()[..4];
        let mut header = vec![0x80 | opcode];
        match payload.len() {
            n @ 0..=125 => header.push(0x80 | n as u8),
            n @ 126..=65535 => {
                header.push(0x80 | 126);
                header.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                header.push(0x80 | 127);
                header.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        header.extend_from_slice(mask);
        self.stream.write_all(&header)?;
        let masked: Vec<u8> = payload
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ mask[i % 4])
            .collect();
        self.stream.write_all(&masked)?;
        Ok(())
    }

    fn send(&mut self, value: &Value) -> Result<()> {
        self.send_frame(1, &serde_json::to_vec(value)?)
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<Value>> {
        // Wait for the beginning of a frame, then allow a complete frame to
        // arrive without losing framing on the ordinary event-poll timeout.
        self.stream
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        let mut first = [0];
        match self.stream.read_exact(&mut first) {
            Ok(()) => (),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        }
        self.stream
            .set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut message = Vec::new();
        let mut opcode = 0;
        loop {
            let mut second = [0];
            self.stream.read_exact(&mut second)?;
            let fin = first[0] & 0x80 != 0;
            let frame_opcode = first[0] & 0x0f;
            let mut length = u64::from(second[0] & 0x7f);
            if length == 126 {
                let mut n = [0; 2];
                self.stream.read_exact(&mut n)?;
                length = u16::from_be_bytes(n).into();
            }
            if length == 127 {
                let mut n = [0; 8];
                self.stream.read_exact(&mut n)?;
                length = u64::from_be_bytes(n);
            }
            if length > MAX_FRAME || message.len() as u64 + length > MAX_FRAME {
                return Err(error("WebSocket frame too large"));
            }
            let mut mask = [0; 4];
            if second[0] & 0x80 != 0 {
                self.stream.read_exact(&mut mask)?;
            }
            let mut bytes = vec![0; length as usize];
            self.stream.read_exact(&mut bytes)?;
            if second[0] & 0x80 != 0 {
                for (i, b) in bytes.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
            }
            match frame_opcode {
                8 => return Err(error("DevTools WebSocket closed")),
                9 => self.send_frame(10, &bytes)?,
                10 => (),
                1 | 2 => {
                    opcode = frame_opcode;
                    message.extend_from_slice(&bytes);
                }
                0 if opcode != 0 => message.extend_from_slice(&bytes),
                _ => return Err(error("Invalid WebSocket frame")),
            }
            if fin && (frame_opcode == 0 || frame_opcode == 1 || frame_opcode == 2) {
                return Ok(Some(serde_json::from_slice(&message)?));
            }
            self.stream.read_exact(&mut first)?;
        }
    }
}

impl Drop for WebSocket {
    fn drop(&mut self) {
        let _ = self.send_frame(8, &[]);
    }
}

pub(super) struct Cdp {
    ws: WebSocket,
    events: VecDeque<Value>,
    counter: u64,
}
impl Cdp {
    pub(super) fn connect(url: &str, timeout: Duration) -> Result<Self> {
        Ok(Self {
            ws: WebSocket::connect(url, timeout)?,
            events: VecDeque::new(),
            counter: 0,
        })
    }
    pub(super) fn send(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<u64> {
        self.counter += 1;
        let mut value = json!({"id":self.counter,"method":method,"params":params});
        if let Some(session) = session {
            value["sessionId"] = json!(session);
        }
        self.ws.send(&value)?;
        Ok(self.counter)
    }
    pub(super) fn response(&mut self, id: u64, timeout: Duration) -> Result<Option<Value>> {
        let deadline = Instant::now() + timeout;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let Some(value) = self.ws.recv(remaining)? else {
                return Ok(None);
            };
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(Some(value));
            }
            if value.get("method").is_some() {
                self.events.push_back(value);
            }
        }
        Ok(None)
    }
    pub(super) fn call(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.send(method, params, session)?;
        let value = self
            .response(id, timeout)?
            .ok_or_else(|| error(format!("{method} timed out")))?;
        if let Some(message) = cdp_error(&value) {
            return Err(error(format!("{method}: {message}")));
        }
        Ok(value.get("result").cloned().unwrap_or_else(|| json!({})))
    }
    pub(super) fn event(&mut self, timeout: Duration) -> Result<Option<Value>> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        let value = self.ws.recv(timeout)?;
        Ok(value.filter(|v| v.get("method").is_some()))
    }
}

pub(super) fn cdp_error(value: &Value) -> Option<String> {
    if let Some(e) = value.get("error").filter(|e| !e.is_null()) {
        return Some(e.to_string());
    }
    value
        .pointer("/result/errorText")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub(super) fn ws_url(value: &Value) -> std::result::Result<&str, Error> {
    value
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| error("Chrome did not provide a DevTools WebSocket URL"))
}
