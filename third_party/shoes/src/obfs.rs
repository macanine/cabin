//! Client-side simple-obfs for Shadowsocks (`plugin: obfs`).
//!
//! The plugin prefixes the raw TCP stream with fake traffic, below the
//! Shadowsocks cipher: an HTTP request in `http` mode, a TLS `ClientHello` in
//! `tls` mode. The obfs server consumes that prefix and forwards the rest.
//! Only the client side is implemented; Cage imports nodes, it never serves.

use std::io;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::address::ResolvedLocation;
use crate::async_stream::{AsyncMessageStream, AsyncStream};
use crate::config::ObfsClientConfig;
use crate::config::ObfsMode;
use crate::tcp::tcp_handler::{TcpClientHandler, TcpClientSetupResult};

/// How long the `http` mode waits for the server's fake HTTP response.
const HTTP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on that response, so a broken server cannot stall the setup.
const HTTP_RESPONSE_LIMIT: usize = 8192;

/// Wraps a Shadowsocks handler with the `obfs` plugin prefix.
#[derive(Debug)]
pub struct ObfsClientHandler {
    inner: Box<dyn TcpClientHandler>,
    mode: ObfsMode,
    host: String,
}

impl ObfsClientHandler {
    pub fn new(inner: Box<dyn TcpClientHandler>, config: &ObfsClientConfig) -> Self {
        Self {
            inner,
            mode: config.mode,
            host: config.host.clone(),
        }
    }

    async fn handshake(&self, stream: &mut Box<dyn AsyncStream>) -> io::Result<()> {
        match self.mode {
            ObfsMode::Http => {
                stream.write_all(&http_request(&self.host)).await?;
                stream.flush().await?;
                read_http_response(stream).await
            }
            ObfsMode::Tls => {
                stream.write_all(&tls_client_hello(&self.host)).await?;
                stream.flush().await
            }
        }
    }
}

#[async_trait]
impl TcpClientHandler for ObfsClientHandler {
    async fn setup_client_tcp_stream(
        &self,
        mut client_stream: Box<dyn AsyncStream>,
        remote_location: ResolvedLocation,
    ) -> io::Result<TcpClientSetupResult> {
        self.handshake(&mut client_stream).await?;
        self.inner
            .setup_client_tcp_stream(client_stream, remote_location)
            .await
    }

    fn supports_udp_over_tcp(&self) -> bool {
        self.inner.supports_udp_over_tcp()
    }

    async fn setup_client_udp_bidirectional(
        &self,
        mut client_stream: Box<dyn AsyncStream>,
        target: ResolvedLocation,
    ) -> io::Result<Box<dyn AsyncMessageStream>> {
        self.handshake(&mut client_stream).await?;
        self.inner
            .setup_client_udp_bidirectional(client_stream, target)
            .await
    }
}

/// A fake but well-formed `GET` request. The path is random so that repeated
/// connections do not share one fingerprint; the server ignores both it and
/// the response it sends back.
fn http_request(host: &str) -> Vec<u8> {
    let path: String = (0..12)
        .map(|_| char::from(b'a' + rand::random::<u8>() % 26))
        .collect();
    format!(
        "GET /{path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64)\r\n\
         Accept: */*\r\n\
         Accept-Encoding: gzip, deflate\r\n\
         Connection: keep-alive\r\n\r\n"
    )
    .into_bytes()
}

/// Reads (and discards) the fake HTTP response that precedes the Shadowsocks
/// stream. Whatever it says is irrelevant; only its end matters.
async fn read_http_response(stream: &mut Box<dyn AsyncStream>) -> io::Result<()> {
    let read = tokio::time::timeout(HTTP_RESPONSE_TIMEOUT, async {
        let mut seen = Vec::with_capacity(64);
        let mut byte = [0u8; 1];
        loop {
            if stream.read(&mut byte).await? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "obfs server closed before its HTTP response",
                ));
            }
            seen.push(byte[0]);
            if seen.ends_with(b"\r\n\r\n") {
                return Ok(());
            }
            if seen.len() > HTTP_RESPONSE_LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "obfs HTTP response too long",
                ));
            }
        }
    })
    .await;
    match read {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "obfs server did not answer the HTTP probe",
        )),
    }
}

fn push_u16(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&(value as u16).to_be_bytes());
}

fn extension(kind: u16, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&kind.to_be_bytes());
    push_u16(&mut out, body.len());
    out.extend_from_slice(body);
    out
}

/// A fake TLS 1.3 `ClientHello` carrying the host as SNI.
///
/// The obfs server parses this only to find the SNI and then drops it, so the
/// message must be structurally valid but need not complete a handshake.
fn tls_client_hello(host: &str) -> Vec<u8> {
    let name = sni_name(host);
    let mut extensions = Vec::new();
    if name.parse::<std::net::IpAddr>().is_err() {
        let mut sni = Vec::with_capacity(5 + name.len());
        push_u16(&mut sni, name.len() + 3);
        sni.push(0); // host_name
        push_u16(&mut sni, name.len());
        sni.extend_from_slice(name.as_bytes());
        extensions.extend_from_slice(&extension(0x0000, &sni));
    }
    // supported_groups: x25519, secp256r1
    extensions.extend_from_slice(&extension(0x000a, &[0x00, 0x04, 0x00, 0x1d, 0x00, 0x17]));
    // ec_point_formats: uncompressed
    extensions.extend_from_slice(&extension(0x000b, &[0x01, 0x00]));
    // signature_algorithms: ecdsa_secp256r1_sha256, rsa_pss_rsae_sha256, rsa_pkcs1_sha256
    extensions.extend_from_slice(&extension(
        0x000d,
        &[0x00, 0x06, 0x04, 0x03, 0x08, 0x04, 0x04, 0x01],
    ));
    // supported_versions: TLS 1.3
    extensions.extend_from_slice(&extension(0x002b, &[0x02, 0x03, 0x04]));

    let mut body = Vec::with_capacity(128);
    body.extend_from_slice(&[0x03, 0x03]); // legacy_version
    body.extend_from_slice(&rand::random::<[u8; 32]>());
    body.push(32);
    body.extend_from_slice(&rand::random::<[u8; 32]>());
    let suites: [u16; 15] = [
        0x1301, 0x1302, 0x1303, 0xc02b, 0xc02f, 0xc02c, 0xc030, 0xcca9, 0xcca8, 0xc013, 0xc014,
        0x009c, 0x009d, 0x002f, 0x0035,
    ];
    push_u16(&mut body, suites.len() * 2);
    for suite in suites {
        body.extend_from_slice(&suite.to_be_bytes());
    }
    body.extend_from_slice(&[0x01, 0x00]); // null compression
    push_u16(&mut body, extensions.len());
    body.extend_from_slice(&extensions);

    let mut handshake = Vec::with_capacity(4 + body.len());
    handshake.push(0x01); // ClientHello
    let length = body.len();
    handshake.extend_from_slice(&[(length >> 16) as u8, (length >> 8) as u8, length as u8]);
    handshake.extend_from_slice(&body);

    let mut record = Vec::with_capacity(5 + handshake.len());
    record.extend_from_slice(&[0x16, 0x03, 0x01]); // handshake, TLS 1.0 record version
    push_u16(&mut record, handshake.len());
    record.extend_from_slice(&handshake);
    record
}

/// Strips an optional `:port` so the SNI stays a host name.
fn sni_name(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') && port.chars().all(|c| c.is_ascii_digit()) => {
            name
        }
        _ => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sni_drops_a_trailing_port() {
        assert_eq!(sni_name("a.example:252587"), "a.example");
        assert_eq!(sni_name("a.example"), "a.example");
        assert_eq!(sni_name("[::1]:8443"), "[::1]:8443");
    }

    #[test]
    fn client_hello_is_structurally_valid() {
        let hello = tls_client_hello("obfs.example:252587");
        assert_eq!(hello[0], 0x16);
        assert_eq!(
            u16::from_be_bytes([hello[3], hello[4]]) as usize,
            hello.len() - 5
        );
        let body = &hello[9..];
        assert_eq!(body[0], 0x03);
        assert_eq!(body[1], 0x03);
        assert_eq!(body[2 + 32], 32, "session id length");

        // The SNI extension, byte for byte: type, length, list length,
        // host_name type, name length, name.
        let name = b"obfs.example";
        let mut expected = vec![0x00, 0x00];
        expected.extend_from_slice(&((name.len() + 5) as u16).to_be_bytes());
        expected.extend_from_slice(&((name.len() + 3) as u16).to_be_bytes());
        expected.push(0x00);
        expected.extend_from_slice(&(name.len() as u16).to_be_bytes());
        expected.extend_from_slice(name);
        assert!(
            hello.windows(expected.len()).any(|window| window == expected),
            "SNI extension for {name:?} not found"
        );
        assert!(
            !String::from_utf8_lossy(&hello).contains("252587"),
            "port leaked into the SNI"
        );
    }

    #[test]
    fn http_request_ends_with_an_empty_line() {
        let request = http_request("obfs.example");
        assert!(request.starts_with(b"GET /"));
        assert!(request.ends_with(b"\r\n\r\n"));
        assert!(String::from_utf8_lossy(&request).contains("Host: obfs.example\r\n"));
    }
}
