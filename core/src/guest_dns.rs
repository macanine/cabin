//! Guest DNS over HTTPS, always through the configured HTTP proxy.
//!
//! `run` is the service entry point. It reads VMCTL_DNS_HOST, VMCTL_DNS_PORT,
//! VMCTL_PROXY_URL and VMCTL_DOH_URL, then serves until a listener fails or the
//! process is stopped. Failed lookups are dropped, as in the original service.

use std::env;
use std::error::Error;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: &str = "53";
const DEFAULT_PROXY: &str = "http://10.0.2.2:18080";
const DEFAULT_DOH: &str = "https://cloudflare-dns.com/dns-query";
const MAX_DNS_MESSAGE: usize = u16::MAX as usize;
const MIN_DNS_MESSAGE: usize = 12;
const MAX_IN_FLIGHT: usize = 32;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(8);
const TCP_TIMEOUT: Duration = Duration::from_secs(10);

/// Run the DNS service using the VMCTL_* environment variables.
///
/// Configuration and listener errors are returned to the caller. Individual
/// malformed queries, busy requests and upstream failures are silently dropped.
pub fn run() -> Result<(), Box<dyn Error + Send + Sync>> {
    let config = Config::from_env()?;
    let upstream = Arc::new(Upstream::new(&config)?);
    let server = Server::bind(&config.host, config.port, upstream)?;
    server.serve(&AtomicBool::new(false))?;
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

struct Config {
    host: String,
    port: u16,
    proxy: String,
    endpoint: String,
}

impl Config {
    fn from_env() -> io::Result<Self> {
        fn get(name: &str, default: &str) -> io::Result<String> {
            match env::var(name) {
                Ok(value) => Ok(value),
                Err(env::VarError::NotPresent) => Ok(default.to_owned()),
                Err(env::VarError::NotUnicode(_)) => {
                    Err(invalid("DNS environment value is not Unicode"))
                }
            }
        }
        let host = get("VMCTL_DNS_HOST", DEFAULT_HOST)?;
        let port = get("VMCTL_DNS_PORT", DEFAULT_PORT)?
            .parse::<u16>()
            .map_err(|_| invalid("invalid VMCTL_DNS_PORT"))?;
        Ok(Self {
            host,
            port,
            proxy: get("VMCTL_PROXY_URL", DEFAULT_PROXY)?,
            endpoint: get("VMCTL_DOH_URL", DEFAULT_DOH)?,
        })
    }
}

struct Upstream {
    agent: ureq::Agent,
    endpoint: String,
}

impl Upstream {
    fn new(config: &Config) -> io::Result<Self> {
        let endpoint =
            url::Url::parse(&config.endpoint).map_err(|_| invalid("invalid VMCTL_DOH_URL"))?;
        if endpoint.scheme() != "https"
            || endpoint.host().is_none()
            || endpoint.fragment().is_some()
        {
            return Err(invalid(
                "VMCTL_DOH_URL must be an HTTPS URL without a fragment",
            ));
        }
        let proxy_url =
            url::Url::parse(&config.proxy).map_err(|_| invalid("invalid VMCTL_PROXY_URL"))?;
        // ureq 2's proxy parser does not support IPv6 literals. Reject them
        // rather than silently connecting to a different address or port.
        if proxy_url.scheme() != "http"
            || proxy_url.host().is_none()
            || matches!(proxy_url.host(), Some(url::Host::Ipv6(_)))
            || proxy_url.port() == Some(0)
            || proxy_url.path() != "/"
            || proxy_url.query().is_some()
            || proxy_url.fragment().is_some()
            || (!proxy_url.username().is_empty() && proxy_url.password().is_none())
        {
            return Err(invalid(
                "VMCTL_PROXY_URL must be a supported HTTP proxy URL",
            ));
        }
        let proxy =
            ureq::Proxy::new(proxy_url.as_str()).map_err(|_| invalid("invalid VMCTL_PROXY_URL"))?;
        let agent = ureq::AgentBuilder::new()
            .proxy(proxy)
            .try_proxy_from_env(false)
            .https_only(true)
            .redirects(0)
            .timeout_connect(LOOKUP_TIMEOUT)
            .timeout(LOOKUP_TIMEOUT)
            .build();
        Ok(Self {
            agent,
            endpoint: endpoint.into(),
        })
    }

    fn lookup(&self, query: &[u8]) -> io::Result<Vec<u8>> {
        if !(MIN_DNS_MESSAGE..=MAX_DNS_MESSAGE).contains(&query.len()) {
            return Err(invalid("invalid DNS query length"));
        }
        let response = self
            .agent
            .post(&self.endpoint)
            .set("Content-Type", "application/dns-message")
            .set("Accept", "application/dns-message")
            .send_bytes(query)
            .map_err(|_| io::Error::other("DNS upstream request failed"))?;
        if !(200..300).contains(&response.status()) {
            return Err(io::Error::other(
                "DNS upstream returned an unsuccessful status",
            ));
        }
        let mut result = Vec::new();
        response
            .into_reader()
            .take((MAX_DNS_MESSAGE + 1) as u64)
            .read_to_end(&mut result)?;
        if !(MIN_DNS_MESSAGE..=MAX_DNS_MESSAGE).contains(&result.len()) || result[..2] != query[..2]
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid DNS response",
            ));
        }
        Ok(result)
    }
}

// The permit is acquired before spawning, so both worker count and in-flight
// upstream requests remain bounded, including clients that send TCP slowly.
struct Slots(AtomicUsize);

impl Slots {
    fn acquire(self: &Arc<Self>) -> Option<Permit> {
        self.0
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |n| {
                (n < MAX_IN_FLIGHT).then_some(n + 1)
            })
            .ok()?;
        Some(Permit(Arc::clone(self)))
    }
}

struct Permit(Arc<Slots>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::Release);
    }
}

struct Server {
    udp: Arc<UdpSocket>,
    tcp: TcpListener,
    upstream: Arc<Upstream>,
    slots: Arc<Slots>,
}

impl Server {
    fn bind(host: &str, port: u16, upstream: Arc<Upstream>) -> io::Result<Self> {
        let udp = UdpSocket::bind((host, port))?;
        // Reuse the assigned port when tests or callers request an ephemeral port.
        let tcp = TcpListener::bind((host, udp.local_addr()?.port()))?;
        udp.set_nonblocking(true)?;
        tcp.set_nonblocking(true)?;
        Ok(Self {
            udp: Arc::new(udp),
            tcp,
            upstream,
            slots: Arc::new(Slots(AtomicUsize::new(0))),
        })
    }

    fn serve(&self, stop: &AtomicBool) -> io::Result<()> {
        let mut buffer = [0; MAX_DNS_MESSAGE];
        while !stop.load(Ordering::Acquire) {
            let mut progressed = false;
            match self.udp.recv_from(&mut buffer) {
                Ok((size, peer)) => {
                    progressed = true;
                    if let Some(permit) = self.slots.acquire() {
                        let query = buffer[..size].to_vec();
                        let udp = Arc::clone(&self.udp);
                        let upstream = Arc::clone(&self.upstream);
                        // Thread creation can fail under resource pressure; in that
                        // case the captured permit is dropped and the request lost.
                        let _ = thread::Builder::new().spawn(move || {
                            let _permit = permit;
                            if let Ok(response) = upstream.lookup(&query) {
                                let _ = udp.send_to(&response, peer);
                            }
                        });
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
            match self.tcp.accept() {
                Ok((stream, _)) => {
                    progressed = true;
                    if let Some(permit) = self.slots.acquire() {
                        let upstream = Arc::clone(&self.upstream);
                        let _ = thread::Builder::new().spawn(move || {
                            let _permit = permit;
                            let _ = handle_tcp(stream, &upstream);
                        });
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
            if !progressed {
                thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }
}

fn read_before(stream: &mut TcpStream, mut data: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !data.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "DNS TCP read timed out"))?;
        stream.set_read_timeout(Some(remaining))?;
        match stream.read(data) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => data = &mut data[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn handle_tcp(mut stream: TcpStream, upstream: &Upstream) -> io::Result<()> {
    let deadline = Instant::now() + TCP_TIMEOUT;
    let mut length = [0; 2];
    read_before(&mut stream, &mut length, deadline)?;
    let size = u16::from_be_bytes(length) as usize;
    if size < MIN_DNS_MESSAGE {
        return Err(invalid("invalid DNS query length"));
    }
    let mut query = vec![0; size];
    read_before(&mut stream, &mut query, deadline)?;
    let response = upstream.lookup(&query)?;
    stream.set_write_timeout(Some(TCP_TIMEOUT))?;
    stream.write_all(&(response.len() as u16).to_be_bytes())?;
    stream.write_all(&response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Shutdown, SocketAddr};
    use std::sync::mpsc;
    use std::thread::JoinHandle;

    fn config(proxy: String, endpoint: String) -> Config {
        Config {
            host: "127.0.0.1".into(),
            port: 0,
            proxy,
            endpoint,
        }
    }

    fn query() -> Vec<u8> {
        vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0]
    }

    fn response() -> Vec<u8> {
        vec![0x12, 0x34, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0]
    }

    // Use a loopback HTTP endpoint only in the test client, to inspect the
    // complete HTTP exchange without adding a TLS server dependency. The
    // production constructor separately enforces HTTPS and TLS validation.
    fn test_upstream(proxy: SocketAddr, timeout: Duration) -> Arc<Upstream> {
        let agent = ureq::AgentBuilder::new()
            .proxy(ureq::Proxy::new(format!("http://{proxy}")).unwrap())
            .try_proxy_from_env(false)
            .redirects(0)
            .timeout_connect(timeout)
            .timeout(timeout)
            .max_idle_connections(0)
            .build();
        Arc::new(Upstream {
            agent,
            endpoint: "http://doh.invalid/dns-query".into(),
        })
    }

    fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
            assert!(headers.len() < 16384);
        }
        let headers = String::from_utf8(headers).unwrap();
        let size: usize = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0; size];
        stream.read_exact(&mut body).unwrap();
        (headers, body)
    }

    /// What the fake proxy received: each request line with its body.
    type Received = Vec<(String, Vec<u8>)>;

    fn fake_proxy(replies: Vec<(u16, Vec<u8>)>) -> (SocketAddr, JoinHandle<Received>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                requests.push(read_request(&mut stream));
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
            requests
        });
        (address, handle)
    }

    struct RunningServer {
        addr: SocketAddr,
        stop: Arc<AtomicBool>,
        slots: Arc<Slots>,
        handle: Option<JoinHandle<io::Result<()>>>,
    }

    impl RunningServer {
        fn start(upstream: Arc<Upstream>) -> Self {
            let server = Server::bind("127.0.0.1", 0, upstream).unwrap();
            let addr = server.udp.local_addr().unwrap();
            let slots = Arc::clone(&server.slots);
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let handle = thread::spawn(move || server.serve(&thread_stop));
            Self {
                addr,
                stop,
                slots,
                handle: Some(handle),
            }
        }
    }

    impl Drop for RunningServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.handle.take().unwrap().join().unwrap().unwrap();
        }
    }

    #[test]
    fn udp_and_tcp_forward_binary_dns_through_the_proxy() {
        let (proxy, proxy_thread) = fake_proxy(vec![(200, response()), (200, response())]);
        let server = RunningServer::start(test_upstream(proxy, Duration::from_secs(2)));
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        udp.send_to(&query(), server.addr).unwrap();
        let mut buf = [0; 512];
        let (len, from) = udp.recv_from(&mut buf).unwrap();
        assert_eq!(from, server.addr);
        assert_eq!(&buf[..len], response());

        let mut tcp = TcpStream::connect(server.addr).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        // Deliberately fragment the length and message across writes.
        tcp.write_all(&[0]).unwrap();
        tcp.write_all(&[query().len() as u8]).unwrap();
        tcp.write_all(&query()[..4]).unwrap();
        tcp.write_all(&query()[4..]).unwrap();
        let mut len = [0; 2];
        tcp.read_exact(&mut len).unwrap();
        let mut body = vec![0; u16::from_be_bytes(len) as usize];
        tcp.read_exact(&mut body).unwrap();
        assert_eq!(body, response());

        let requests = proxy_thread.join().unwrap();
        for (headers, body) in requests {
            assert!(
                headers.starts_with("POST http://doh.invalid/dns-query HTTP/1.1\r\n"),
                "{headers}"
            );
            let headers = headers.to_ascii_lowercase();
            assert!(headers.contains("content-type: application/dns-message\r\n"));
            assert!(headers.contains("accept: application/dns-message\r\n"));
            assert_eq!(body, query());
        }
    }

    #[test]
    fn invalid_queries_responses_and_statuses_are_rejected() {
        let mut wrong_id = response();
        wrong_id[1] ^= 1;
        let cases = vec![
            (200, vec![0; 11]),
            (200, wrong_id),
            (200, vec![0; MAX_DNS_MESSAGE + 1]),
            (500, response()),
            (302, response()),
        ];
        let (proxy, proxy_thread) = fake_proxy(cases);
        let upstream = test_upstream(proxy, Duration::from_secs(2));
        assert!(upstream.lookup(&[0; 11]).is_err());
        assert!(upstream.lookup(&vec![0; MAX_DNS_MESSAGE + 1]).is_err());
        for _ in 0..5 {
            assert!(upstream.lookup(&query()).is_err());
        }
        assert_eq!(proxy_thread.join().unwrap().len(), 5);
    }

    #[test]
    fn tcp_truncation_and_invalid_lengths_do_not_reach_upstream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = RunningServer::start(test_upstream(
            listener.local_addr().unwrap(),
            Duration::from_secs(1),
        ));
        for bytes in [&[0, 11][..], &[0, 12, 1, 2][..]] {
            let mut client = TcpStream::connect(server.addr).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            client.write_all(bytes).unwrap();
            client.shutdown(Shutdown::Write).unwrap();
            let mut b = [0];
            assert_eq!(client.read(&mut b).unwrap(), 0);
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn production_config_requires_https_and_a_valid_http_proxy() {
        for endpoint in [
            "http://example.com/dns-query",
            "https://example.com/#fragment",
            "not a url",
        ] {
            assert!(Upstream::new(&config(DEFAULT_PROXY.into(), endpoint.into())).is_err());
        }
        for proxy in [
            "",
            "socks5://127.0.0.1:1080",
            "https://127.0.0.1:443",
            "http://127.0.0.1:0",
            "http://[::1]:8080",
            "http://localhost:8080/path",
            "http://localhost:8080/?x=1",
            "http://user@localhost:8080",
        ] {
            assert!(
                Upstream::new(&config(proxy.into(), DEFAULT_DOH.into())).is_err(),
                "{proxy}"
            );
        }
        assert!(Upstream::new(&config(DEFAULT_PROXY.into(), DEFAULT_DOH.into())).is_ok());
    }

    #[test]
    fn https_uses_connect_and_rejects_invalid_tls() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let proxy = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (connect, _) = read_request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .unwrap();
            let mut tls_header = [0; 5];
            stream.read_exact(&mut tls_header).unwrap();
            // This is not a valid server handshake or certificate.
            stream.write_all(b"not TLS").unwrap();
            (connect, tls_header)
        });
        let upstream = Upstream::new(&config(
            format!("http://{address}"),
            "https://example.invalid/dns-query".into(),
        ))
        .unwrap();
        assert!(upstream.lookup(&query()).is_err());
        let (connect, tls_header) = proxy.join().unwrap();
        assert!(
            connect.starts_with("CONNECT example.invalid:443 HTTP/1.1\r\n"),
            "{connect}"
        );
        assert_eq!(tls_header[0], 0x16); // TLS handshake record
        assert_eq!(tls_header[1], 0x03);
    }

    #[test]
    fn proxy_failure_never_falls_back_to_the_endpoint() {
        let endpoint = TcpListener::bind("127.0.0.1:0").unwrap();
        endpoint.set_nonblocking(true).unwrap();
        let unavailable = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_address = unavailable.local_addr().unwrap();
        drop(unavailable);
        let upstream = Upstream::new(&config(
            format!("http://{proxy_address}"),
            format!("https://{}/dns-query", endpoint.local_addr().unwrap()),
        ))
        .unwrap();
        assert!(upstream.lookup(&query()).is_err());
        assert_eq!(
            endpoint.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn stalled_proxy_times_out_and_releases_its_slot() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let proxy = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            accepted_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(500));
        });
        let server = RunningServer::start(test_upstream(proxy_addr, Duration::from_millis(120)));
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(Duration::from_millis(350)))
            .unwrap();
        udp.send_to(&query(), server.addr).unwrap();
        accepted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let mut buf = [0; 512];
        assert!(udp.recv_from(&mut buf).is_err());
        assert_eq!(server.slots.0.load(Ordering::Acquire), 0);
        proxy.join().unwrap();
    }

    #[test]
    fn slow_clients_are_bounded_and_release_capacity() {
        let slots = Arc::new(Slots(AtomicUsize::new(0)));
        let held: Vec<_> = (0..MAX_IN_FLIGHT)
            .map(|_| slots.acquire().expect("a slot"))
            .collect();
        assert!(slots.acquire().is_none(), "the in-flight bound is exact");
        drop(held);
        assert!(slots.acquire().is_some(), "capacity returns after release");
    }
}
