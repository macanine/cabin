//! Guest DNS: query checks, the DoH resolver with its pool and hedging, and
//! the bootstrap servers used on a bound adapter.

use super::chain::{Upstream, client_chain};
use super::relay::MAX_DNS_QUERY;
use super::MTU;
use crate::util::LockExt;
use std::io::Read;
use std::net::{Ipv4Addr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shoes::embed::{HttpRelay, RelayTicket};

use crate::{Error, Result};

/// Offset after one uncompressed DNS question, and its type and class.
pub(super) fn dns_question(query: &[u8]) -> Option<(usize, u16, u16)> {
    if query.len() < 17 || query[2] & 0xf8 != 0 || query[4..6] != [0, 1]
        || query[6..10] != [0, 0, 0, 0] {
        return None;
    }
    let mut pos = 12;
    let mut name_len = 0usize;
    loop {
        let len = *query.get(pos)? as usize;
        pos += 1;
        name_len += len + 1;
        if len > 63 || name_len > 255 { return None; }
        if len == 0 { break; }
        pos = pos.checked_add(len)?;
        if pos > query.len() { return None; }
    }
    let end = pos.checked_add(4)?;
    let fields = query.get(pos..end)?;
    Some((end, u16::from_be_bytes([fields[0], fields[1]]), u16::from_be_bytes([fields[2], fields[3]])))
}

/// Keep one Internet question and a bare EDNS record when requested. EDNS
/// options may contain client addresses or other data the resolver needn't see.
pub(super) fn sanitize_dns_query(query: &mut Vec<u8>) -> bool {
    let Some((end, _, 1)) = dns_question(query) else { return false };
    let opt = if query[10..12] == [0, 1] {
        query.get(end..end + 11).filter(|record| {
            record[..3] == [0, 0, 41]
                && record[5..7] == [0, 0]
                && query.len() == end + 11 + usize::from(u16::from_be_bytes([record[9], record[10]]))
        }).map(|record| {
            let size = u16::from_be_bytes([record[3], record[4]]).clamp(512, 1232).to_be_bytes();
            [0, 0, 41, size[0], size[1], 0, 0, record[7] & 0x80, 0, 0, 0]
        })
    } else {
        None
    };
    query.truncate(end);
    query[2] &= 1;
    query[3] &= 0x10; // checking disabled
    query[10..12].fill(0);
    if let Some(opt) = opt {
        query[11] = 1;
        query.extend_from_slice(&opt);
    }
    true
}

/// Only accept a DNS response to the question we sent; retry transient server failures.
fn valid_doh_response(query: &[u8], response: &[u8]) -> bool {
    let Some((end, _, _)) = dns_question(query) else { return false; };
    response.len() >= end
        && response[..2] == query[..2]
        && response[2] & 0xf8 == 0x80
        && matches!(response[3] & 0x0f, 0 | 3)
        && response[4..6] == [0, 1]
        && response[12..end].eq_ignore_ascii_case(&query[12..end])
}

/// Answer IPv6 address queries locally; no IPv6 answer or upstream request.
pub(super) fn dns_ipv6_nodata(query: &[u8]) -> Option<Vec<u8>> {
    let (end, kind, _) = dns_question(query)?;
    if kind != 28 { return None; }
    let mut answer = query[..end].to_vec();
    answer[2] = 0x80 | (query[2] & 1); // response, preserve recursion desired
    answer[3] = 0x80; // recursion available, NOERROR
    answer[6..12].fill(0);
    Some(answer)
}

/// Public UDP resolvers for bound sockets, after the interface's own servers.
const BOUND_DNS_FALLBACK: [Ipv4Addr; 3] = [
    Ipv4Addr::new(223, 5, 5, 5),
    Ipv4Addr::new(119, 29, 29, 29),
    Ipv4Addr::new(1, 1, 1, 1),
];

/// UDP servers that resolve DoH and proxy node names on a bound adapter,
/// bypassing any TUN. Networks that block public resolvers still answer
/// through the servers macOS has configured for that adapter.
pub(super) fn bound_dns(upstream: &Upstream) -> Result<Option<shoes::embed::BoundDns>> {
    upstream.interface().map(|name| {
        let servers = bootstrap_dns(&interface_dns(name))
            .into_iter().map(|ip| std::net::SocketAddr::from((ip, 53))).collect();
        shoes::embed::BoundDns::new(name.to_string(), servers).map_err(Error::Io)
    }).transpose()
}

/// The interface's usable servers first, then the public fallback, once each.
fn bootstrap_dns(interface: &[Ipv4Addr]) -> Vec<Ipv4Addr> {
    let mut servers = Vec::new();
    for &ip in interface.iter().filter(|ip| usable_bootstrap_dns(**ip)).chain(&BOUND_DNS_FALLBACK) {
        if !servers.contains(&ip) {
            servers.push(ip);
        }
    }
    servers
}

/// Excludes local and fake-IP (198.18.0.0/15) servers, which a TUN answers.
fn usable_bootstrap_dns(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_loopback() || ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast()
        || (a == 198 && b & 0xfe == 18))
}

/// IPv4 nameservers of the `scutil --dns` resolvers scoped to `interface`.
/// Resolvers for a single domain (`domain :`) are skipped.
#[cfg(any(target_os = "macos", test))]
fn scutil_interface_dns(output: &str, interface: &str) -> Vec<Ipv4Addr> {
    #[derive(Default)]
    struct Entry { servers: Vec<Ipv4Addr>, matches: bool, domain: bool }
    let mut found = Vec::new();
    let mut entry = Entry::default();
    let mut finish = |entry: &mut Entry| {
        let entry = std::mem::take(entry);
        if entry.matches && !entry.domain {
            found.extend(entry.servers);
        }
    };
    for line in output.lines().map(str::trim) {
        if line.starts_with("resolver #") || line.starts_with("DNS configuration") {
            finish(&mut entry);
            continue;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        let (key, value) = (key.trim(), value.trim());
        if key.starts_with("nameserver[") {
            // IPv6 values split at their first colon and fail to parse.
            if let Ok(ip) = value.parse() { entry.servers.push(ip); }
        } else if key == "if_index" {
            entry.matches = value.split_once('(')
                .and_then(|(_, name)| name.strip_suffix(')'))
                .is_some_and(|name| name == interface);
        } else if key == "domain" {
            entry.domain = true;
        }
    }
    finish(&mut entry);
    found
}

/// The DNS servers macOS has configured for `interface`, cached briefly.
/// `scutil` gets 200 ms; on timeout the public fallback is used alone.
#[cfg(target_os = "macos")]
fn interface_dns(interface: &str) -> Vec<Ipv4Addr> {
    use std::time::Instant;
    const FRESH: Duration = Duration::from_secs(30);
    static CACHE: Mutex<Vec<(String, Instant, Vec<Ipv4Addr>)>> = Mutex::new(Vec::new());
    if let Some((_, _, servers)) = CACHE.locked().iter()
        .find(|(name, at, _)| name == interface && at.elapsed() < FRESH)
    {
        return servers.clone();
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new().name("vm-net-scutil".into()).spawn(move || {
        let output = std::process::Command::new("/usr/sbin/scutil").arg("--dns")
            .stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output();
        let _ = sender.send(output);
    });
    let servers = match receiver.recv_timeout(Duration::from_millis(200)) {
        Ok(Ok(output)) if output.status.success() => {
            scutil_interface_dns(&String::from_utf8_lossy(&output.stdout), interface)
        }
        _ => Vec::new(),
    };
    let mut cache = CACHE.locked();
    cache.retain(|(name, at, _)| name != interface && at.elapsed() < FRESH);
    cache.push((interface.to_string(), Instant::now(), servers.clone()));
    servers
}

#[cfg(not(target_os = "macos"))]
fn interface_dns(_interface: &str) -> Vec<Ipv4Addr> {
    Vec::new()
}

/// DNS-over-HTTPS endpoints. A lookup starts with the one that answered last.
const DOH_ENDPOINTS: [&str; 3] = [
    "https://cloudflare-dns.com/dns-query",
    "https://dns.alidns.com/dns-query",
    "https://doh.pub/dns-query",
];
/// How long the preferred endpoint runs alone before the others join. A
/// pooled connection answers in one round trip, well inside this even through
/// a distant exit. A cold connection through a proxy needs about 1.5 s for
/// TCP, proxy and TLS handshakes; hedging at 500 ms races every endpoint's
/// handshake and still fits one full cold handshake inside the budget.
const DOH_HEDGE: Duration = Duration::from_millis(500);
/// The whole lookup, inside the guest's 3 s resolver and probe timeouts.
const DOH_BUDGET: Duration = Duration::from_millis(2500);
/// HTTP attempts in flight across lookups. `DNS_INFLIGHT` bounds lookups, and
/// this bounds the hedged attempts, which may run until the budget ends.
const DOH_ATTEMPTS: usize = 48;
/// Idle keep-alive connections kept per endpoint.
const DOH_IDLE_PER_HOST: usize = 4;
/// `. IN NS`, sent to open pooled connections before the guest asks.
const DOH_WARM_QUERY: [u8; 17] = [0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 1];

/// Keep-alive DoH connections for one DNS policy epoch. Every connection
/// is relayed under the ticket that created the pool, so the gate cuts it, and
/// its upstream flow, as soon as the epoch changes. The next lookup with a
/// current ticket builds a new pool.
///
/// The client connects to a loopback listener per endpoint host, which
/// tunnels through the resolver's relay with CONNECT. ureq 2 never reuses
/// connections made through its own proxy support: it returns them to the
/// pool under a key without the proxy and looks them up with it.
struct DohPool {
    ticket: RelayTicket,
    agent: ureq::Agent,
    accept: Vec<tokio::task::AbortHandle>,
}

impl Drop for DohPool {
    fn drop(&mut self) {
        for task in &self.accept {
            task.abort();
        }
    }
}

/// `host:port` of an `https://` endpoint, as ureq passes it to its resolver.
fn doh_netloc(endpoint: &str) -> Option<String> {
    let authority = endpoint.strip_prefix("https://")?.split('/').next()?;
    if authority.is_empty() {
        None
    } else if authority.contains(':') {
        Some(authority.to_string())
    } else {
        Some(format!("{authority}:443"))
    }
}

/// Carry `client` to `target` through one CONNECT on `relay`.
async fn doh_tunnel(relay: HttpRelay, mut client: tokio::net::TcpStream, target: Arc<str>) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let mut proxy = tokio::net::TcpStream::connect(listener.local_addr()?).await?;
    let (server, peer) = listener.accept().await?;
    if peer != proxy.local_addr()? {
        return Err(std::io::Error::other("unexpected loopback peer"));
    }
    drop(listener);
    let tunnel = async {
        proxy.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes()).await?;
        // Read the reply byte by byte so no tunnel data is consumed.
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8];
            if head.len() > 4096 || proxy.read(&mut byte).await? == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
            }
            head.push(byte[0]);
        }
        if !(head.starts_with(b"HTTP/1.1 200") || head.starts_with(b"HTTP/1.0 200")) {
            return Err(std::io::Error::other("the upstream refused the DoH connection"));
        }
        tokio::io::copy_bidirectional(&mut client, &mut proxy).await.map(|_| ())
    };
    tokio::select! {
        result = relay.relay(server) => result,
        result = tunnel => result,
    }
}

/// Local shoes listener used solely to resolve DNS over HTTPS through the
/// same upstream as the packet stack. This also works with HTTP proxies,
/// which cannot carry UDP DNS.
///
/// A resolver lives for one upstream: `Gateway::set_upstream` and
/// `clear_upstream` replace it, and dropping it stops its runtime and every
/// relay. Egress policy changes within an upstream bump the DNS gate's epoch,
/// which cuts the pooled connections (see [`DohPool`]).
pub(super) struct Resolver {
    pub(super) port: u16,
    preferred_doh: AtomicUsize,
    endpoints: Vec<String>,
    /// Adjusts the HTTP client; tests use it to replace TLS.
    configure: fn(ureq::AgentBuilder) -> ureq::AgentBuilder,
    pool: Mutex<Option<Arc<DohPool>>>,
    attempts: Arc<AtomicUsize>,
    /// Open pooled connections when a policy resumes guest DNS.
    pub(super) warm_on_resume: bool,
    relay: HttpRelay,
    runtime: tokio::runtime::Handle,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Resolver {
    pub(super) fn start(upstream: &Upstream) -> Result<Self> {
        let mut resolver = Self::start_with(upstream, DOH_ENDPOINTS.map(String::from).to_vec(), |agent| agent)?;
        // Gateway tests count upstream connections; they would see warm-ups.
        resolver.warm_on_resume = !cfg!(test);
        Ok(resolver)
    }

    fn start_with(
        upstream: &Upstream,
        endpoints: Vec<String>,
        configure: fn(ureq::AgentBuilder) -> ureq::AgentBuilder,
    ) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let chain = serde_json::to_value(client_chain(upstream)?)
            .map_err(|e| Error::Message(e.to_string()))?;
        let config = serde_yaml::to_string(&serde_json::json!([{
            "address": format!("127.0.0.1:{port}"),
            "protocol": {"type": "http"},
            "rules": [{"masks": ["0.0.0.0/0"], "action": "allow", "client_chain": chain}]
        }]))
        .map_err(|e| Error::Message(e.to_string()))?;
        let dns = bound_dns(upstream)?;
        let (shutdown, mut rx) = tokio::sync::oneshot::channel();
        let (ready, initialized) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("vm-dns-proxy".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => { let _ = ready.send(Err(error)); return; }
                };
                runtime.block_on(async move {
                    let setup = async {
                        let relay = if let Some(dns) = dns {
                            HttpRelay::new_with_bound_dns(&config, dns).await?
                        } else {
                            HttpRelay::new(&config).await?
                        };
                        Ok::<_, std::io::Error>((tokio::net::TcpListener::from_std(listener)?, relay))
                    }.await;
                    let (listener, host_relay) = match setup {
                        Ok(setup) => setup,
                        Err(error) => { let _ = ready.send(Err(error)); return; }
                    };
                    if ready.send(Ok((host_relay.clone(), tokio::runtime::Handle::current()))).is_err() {
                        return;
                    }
                    let mut tasks = tokio::task::JoinSet::new();
                    loop {
                        tokio::select! {
                            _ = &mut rx => break,
                            _ = tasks.join_next(), if !tasks.is_empty() => {},
                            conn = listener.accept() => {
                                match conn {
                                    Ok((stream, _)) => {
                                        let relay = host_relay.clone();
                                        tasks.spawn(async move { let _ = relay.relay(stream).await; });
                                    }
                                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                                }
                            }
                        }
                    }
                    tasks.shutdown().await;
                });
            })?;
        let (relay, runtime) = initialized
            .recv()
            .map_err(|_| std::io::Error::other("DNS proxy failed to start"))??;
        Ok(Self {
            port,
            preferred_doh: AtomicUsize::new(if matches!(upstream, Upstream::BoundDirect(_)) { 1 } else { 0 }),
            endpoints,
            configure,
            pool: Mutex::new(None),
            attempts: Arc::new(AtomicUsize::new(0)),
            warm_on_resume: false,
            relay,
            runtime,
            shutdown: Some(shutdown),
            thread: Some(thread),
        })
    }

    /// The connection pool for `ticket`'s epoch. Tickets all come from the
    /// gateway's one DNS gate, and only one epoch of a gate is current, so a
    /// pool whose own ticket is current serves the same epoch.
    fn pool(&self, ticket: &RelayTicket) -> Option<Arc<DohPool>> {
        let mut slot = self.pool.locked();
        if let Some(pool) = slot.as_ref().filter(|pool| pool.ticket.is_current()) {
            return Some(pool.clone());
        }
        // The gate has already cut a stale pool's connections.
        *slot = None;
        if !ticket.is_current() {
            return None;
        }
        let mut routes: Vec<(String, std::net::SocketAddr)> = Vec::new();
        let mut accept = Vec::new();
        for netloc in self.endpoints.iter().filter_map(|endpoint| doh_netloc(endpoint)) {
            if routes.iter().any(|(known, _)| *known == netloc) {
                continue;
            }
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
            listener.set_nonblocking(true).ok()?;
            routes.push((netloc.clone(), listener.local_addr().ok()?));
            let target: Arc<str> = netloc.into();
            let relay = self.relay.clone();
            let (gate, connections) = (ticket.clone(), ticket.clone());
            accept.push(self.runtime.spawn(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
                gate.run(async move {
                    loop {
                        let Ok((stream, _)) = listener.accept().await else {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        };
                        let (relay, ticket, target) = (relay.clone(), connections.clone(), target.clone());
                        tokio::spawn(async move { let _ = ticket.run(doh_tunnel(relay, stream, target)).await; });
                    }
                }).await;
            }).abort_handle());
        }
        // Only the endpoints resolve, each to its tunnel: no system DNS.
        let resolve = move |netloc: &str| {
            routes.iter().find(|(known, _)| known == netloc).map(|(_, addr)| vec![*addr])
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "not a DoH endpoint"))
        };
        let agent = (self.configure)(
            ureq::AgentBuilder::new()
                .resolver(resolve)
                .try_proxy_from_env(false)
                .redirects(0)
                .max_idle_connections_per_host(DOH_IDLE_PER_HOST),
        ).build();
        let pool = Arc::new(DohPool { ticket: ticket.clone(), agent, accept });
        *slot = Some(pool.clone());
        Some(pool)
    }

    /// Open the preferred endpoint's connection for this epoch in the
    /// background, so the guest's first query costs one round trip.
    pub(super) fn warm(self: &Arc<Self>, ticket: RelayTicket) -> Option<std::thread::JoinHandle<()>> {
        if self.pool.locked().as_ref().is_some_and(|pool| pool.ticket.is_current()) {
            return None;
        }
        let resolver = self.clone();
        std::thread::Builder::new()
            .name("vm-net-doh".into())
            .spawn(move || { resolver.lookup(&ticket, &DOH_WARM_QUERY); })
            .ok()
    }

    pub(super) fn lookup(&self, ticket: &RelayTicket, query: &[u8]) -> Option<Vec<u8>> {
        use base64::Engine;
        if !(12..=MAX_DNS_QUERY).contains(&query.len()) || !ticket.is_current() {
            return None;
        }
        // Every attempt goes through this upstream under this policy epoch.
        // There is no system-DNS or direct fallback if an upstream or TLS
        // check fails.
        let pool = self.pool(ticket)?;
        let deadline = std::time::Instant::now() + DOH_BUDGET;
        let count = self.endpoints.len();
        let preferred = self.preferred_doh.load(Ordering::Relaxed) % count;
        // GET is idempotent, so the client retries a request on a fresh
        // connection when the server has closed a pooled one.
        let dns: Arc<str> = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(query).into();
        let (answers, results) = std::sync::mpsc::channel();
        let (mut started, mut pending) = (0, 0);
        let mut hedge = Some(std::time::Instant::now() + DOH_HEDGE);
        loop {
            // The preferred endpoint goes first. The others join after the
            // hedge delay, or at once when an attempt fails.
            while started < count && (started == 0 || hedge.is_none()) {
                let index = (preferred + started) % count;
                if self.attempt(&pool, index, &dns, deadline, started == 0, answers.clone()) {
                    pending += 1;
                }
                started += 1;
            }
            if pending == 0 {
                if started == count { return None; }
                hedge = None;
                continue;
            }
            let wake = hedge.map_or(deadline, |hedge| hedge.min(deadline));
            match results.recv_timeout(wake.saturating_duration_since(std::time::Instant::now())) {
                Ok((index, response)) => {
                    pending -= 1;
                    if let Some(mut bytes) = response.filter(|bytes| valid_doh_response(query, bytes)) {
                        if !ticket.is_current() { return None; }
                        self.preferred_doh.store(index, Ordering::Relaxed);
                        if bytes.len() > MTU - 28 {
                            // Signal truncation so a DNS client can retry over TCP.
                            bytes.truncate(12);
                            bytes[2] |= 0x02;
                            bytes[4..12].fill(0);
                        }
                        return Some(bytes);
                    }
                    hedge = None;
                }
                Err(_) if std::time::Instant::now() >= deadline => return None,
                Err(_) => hedge = None,
            }
            if !ticket.is_current() {
                return None;
            }
        }
    }

    /// Query one endpoint on its own thread and report `(index, body)`. Only
    /// the preferred endpoint may exceed the shared attempt limit.
    fn attempt(
        &self,
        pool: &Arc<DohPool>,
        index: usize,
        dns: &str,
        deadline: std::time::Instant,
        preferred: bool,
        answers: std::sync::mpsc::Sender<(usize, Option<Vec<u8>>)>,
    ) -> bool {
        let timeout = deadline.saturating_duration_since(std::time::Instant::now());
        if timeout.is_zero() {
            return false;
        }
        if self.attempts.fetch_add(1, Ordering::SeqCst) >= DOH_ATTEMPTS && !preferred {
            self.attempts.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        let (pool, attempts) = (pool.clone(), self.attempts.clone());
        let url = format!("{}?dns={dns}", self.endpoints[index]);
        let spawned = std::thread::Builder::new().name("vm-net-doh".into()).spawn(move || {
            let response = pool.agent.get(&url)
                .set("Accept", "application/dns-message")
                .timeout(timeout)
                .call();
            let mut bytes = Vec::new();
            // Reading the body to its end returns the connection to the pool.
            let body = response.ok().filter(|response| response.status() == 200).and_then(|response| {
                response.into_reader().take(65536).read_to_end(&mut bytes).ok()
            });
            attempts.fetch_sub(1, Ordering::SeqCst);
            let _ = answers.send((index, body.map(|_| bytes)));
        });
        if spawned.is_err() {
            self.attempts.fetch_sub(1, Ordering::SeqCst);
        }
        spawned.is_ok()
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::Instant;

    use super::*;
    use crate::netgw::EgressMode;
    use shoes::embed::RelayGate;

    #[test]
    fn aaaa_queries_are_answered_without_forwarding() {
        let mut query = vec![0u8; 12];
        query[..2].copy_from_slice(&[0x12, 0x34]);
        query[2] = 1;
        query[5] = 1;
        query.extend_from_slice(b"\x03one\x03one\x03one\x03one\x00\x00\x1c\x00\x01");
        for mode in [EgressMode::Open, EgressMode::Install, EgressMode::Checking, EgressMode::Rechecking] {
            let mut accepted = query.clone();
            assert!(mode.allows_dns(&mut accepted));
            let answer = dns_ipv6_nodata(&accepted).unwrap();
            assert_eq!(&answer[..2], &[0x12, 0x34]);
            assert_eq!(&answer[2..4], &[0x81, 0x80]);
            assert_eq!(&answer[4..12], &[0, 1, 0, 0, 0, 0, 0, 0]);
            assert_eq!(&answer[12..], &query[12..]);
        }
        assert!(!EgressMode::Closed.allows_dns(&mut query));
        assert!(!EgressMode::Suspended.allows_dns(&mut query));
        let mut a = query.clone();
        let end = a.len();
        a[end - 3] = 1;
        assert!(dns_ipv6_nodata(&a).is_none());
        assert!(EgressMode::Open.allows_dns(&mut a));
        query[5] = 2;
        assert!(!EgressMode::Open.allows_dns(&mut query));
    }

    #[test]
    fn ordinary_dns_strips_edns_options_and_preserves_dnssec_request() {
        let mut query = vec![0u8; 12];
        query[5] = 1;
        query[11] = 1;
        query.extend_from_slice(b"\x07example\x03com\x00\x00\x01\x00\x01");
        let end = query.len();
        query.extend_from_slice(&[0, 0, 41, 16, 0, 0, 0, 128, 0, 0, 4, 0, 8, 0, 0]);
        assert!(sanitize_dns_query(&mut query));
        assert_eq!(&query[end..], &[0, 0, 41, 4, 208, 0, 0, 128, 0, 0, 0]);
        assert_eq!(query[11], 1);
        query.push(1); // A trailing record without a length is discarded.
        assert!(sanitize_dns_query(&mut query));
        assert_eq!(query.len(), end);
        assert_eq!(query[11], 0);
        query[end - 1] = 3; // CHAOS queries are not Internet DNS.
        assert!(!sanitize_dns_query(&mut query));
    }

    #[test]
    fn doh_response_must_match_the_query_and_not_be_a_server_error() {
        let mut query = vec![0u8; 12];
        query[0..2].copy_from_slice(&[0x12, 0x34]);
        query[4..6].copy_from_slice(&1u16.to_be_bytes());
        query.extend_from_slice(b"\x03one\x03one\x03one\x03one\x00\x00\x01\x00\x01");
        let mut response = query.clone();
        response[2] = 0x81;
        response[3] = 0x80;
        assert!(valid_doh_response(&query, &response));
        response[3] = 0x82;
        assert!(!valid_doh_response(&query, &response));
        response[3] = 0x80;
        response[0] ^= 1;
        assert!(!valid_doh_response(&query, &response));
        response[0] ^= 1;
        response[13] ^= 1;
        assert!(!valid_doh_response(&query, &response));
        assert!(!valid_doh_response(&query, &response[..12]));
    }

    #[test]
    fn bound_bootstrap_dns_prefers_the_interfaces_own_servers() {
        let output = "\
DNS configuration

resolver #1
  nameserver[0] : 198.18.0.2
  if_index : 29 (utun1024)
  flags    : Request A records
  reach    : 0x00000003 (Reachable,Transient Connection)

resolver #2
  domain   : corp.example
  nameserver[0] : 10.196.0.53
  if_index : 16 (en0)

resolver #3
  domain   : local
  options  : mdns

DNS configuration (for scoped queries)

resolver #1
  nameserver[0] : 114.114.114.114
  nameserver[1] : fe80::1%en0
  nameserver[2] : 127.0.0.1
  nameserver[3] : 223.5.5.5
  if_index : 16 (en0)
  flags    : Scoped, Request A records
  reach    : 0x00020002 (Reachable,Directly Reachable Address)

resolver #2
  nameserver[0] : 198.18.0.2
  if_index : 29 (utun1024)
  flags    : Scoped, Request A records
";
        let en0 = scutil_interface_dns(output, "en0");
        assert_eq!(en0, [Ipv4Addr::new(114, 114, 114, 114), Ipv4Addr::LOCALHOST, Ipv4Addr::new(223, 5, 5, 5)]);
        assert!(scutil_interface_dns(output, "en1").is_empty());
        assert!(scutil_interface_dns(output, "en").is_empty());
        let mut expected = vec![Ipv4Addr::new(114, 114, 114, 114)];
        expected.extend(BOUND_DNS_FALLBACK);
        assert_eq!(bootstrap_dns(&en0), expected);
        // A TUN's fake-IP resolver is never a bootstrap server.
        assert_eq!(bootstrap_dns(&scutil_interface_dns(output, "utun1024")), BOUND_DNS_FALLBACK);
        assert!(!usable_bootstrap_dns(Ipv4Addr::new(198, 19, 255, 1)));
        assert!(usable_bootstrap_dns(Ipv4Addr::new(198, 20, 0, 1)));
    }

    /// Lets `https://` endpoints run as plain HTTP inside the proxy tunnel.
    struct PlainTls;

    impl ureq::TlsConnector for PlainTls {
        fn connect(&self, _: &str, io: Box<dyn ureq::ReadWrite>) -> std::result::Result<Box<dyn ureq::ReadWrite>, ureq::Error> {
            Ok(io)
        }
    }

    fn plain_tls(agent: ureq::AgentBuilder) -> ureq::AgentBuilder {
        agent.tls_connector(Arc::new(PlainTls))
    }

    #[derive(Clone, Copy, PartialEq)]
    enum DohBehavior { Answer, Stall, Fail }

    /// A loopback DoH server over HTTP/1.1 keep-alive that counts connections.
    struct FakeDoh {
        url: String,
        opened: Arc<AtomicUsize>,
        closed: Arc<AtomicUsize>,
    }

    impl FakeDoh {
        fn start(behavior: DohBehavior) -> Self {
            use base64::Engine;
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let url = format!("https://{}/dns-query", listener.local_addr().unwrap());
            let (opened, closed) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
            let (accepted, finished) = (opened.clone(), closed.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { return };
                    accepted.fetch_add(1, Ordering::SeqCst);
                    let finished = finished.clone();
                    std::thread::spawn(move || {
                        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                        let mut writer = stream;
                        loop {
                            let mut request = String::new();
                            let mut line = String::new();
                            loop {
                                line.clear();
                                match std::io::BufRead::read_line(&mut reader, &mut line) {
                                    Ok(0) | Err(_) => { finished.fetch_add(1, Ordering::SeqCst); return; }
                                    Ok(_) if line == "\r\n" => break,
                                    Ok(_) => request.push_str(&line),
                                }
                            }
                            if behavior == DohBehavior::Stall {
                                continue;
                            }
                            let dns = request.split_once("?dns=").unwrap().1.split_once(' ').unwrap().0;
                            let mut answer = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(dns).unwrap();
                            answer[2] |= 0x80;
                            answer[3] = 0x80;
                            let (status, body) = match behavior {
                                DohBehavior::Fail => ("503 Unavailable", Vec::new()),
                                _ => ("200 OK", answer),
                            };
                            let head = format!(
                                "HTTP/1.1 {status}\r\nContent-Type: application/dns-message\r\nContent-Length: {}\r\n\r\n",
                                body.len(),
                            );
                            if writer.write_all(head.as_bytes()).and_then(|_| writer.write_all(&body)).is_err() {
                                return;
                            }
                        }
                    });
                }
            });
            Self { url, opened, closed }
        }

        fn resolver(endpoints: &[&Self]) -> Resolver {
            let endpoints = endpoints.iter().map(|server| server.url.clone()).collect();
            Resolver::start_with(&Upstream::Direct, endpoints, plain_tls).unwrap()
        }
    }

    fn a_query(id: u16) -> Vec<u8> {
        let mut query = id.to_be_bytes().to_vec();
        query.extend_from_slice(&[1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        query.extend_from_slice(b"\x07example\x03com\x00\x00\x01\x00\x01");
        query
    }

    fn wait_for(what: &str, condition: impl Fn() -> bool) {
        let start = Instant::now();
        while !condition() {
            assert!(start.elapsed() < Duration::from_secs(3), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn doh_lookups_reuse_one_connection_until_the_policy_epoch_changes() {
        let server = FakeDoh::start(DohBehavior::Answer);
        let resolver = Arc::new(FakeDoh::resolver(&[&server]));
        let gate = RelayGate::default();
        let ticket = gate.ticket().unwrap();
        resolver.warm(ticket.clone()).unwrap().join().unwrap();
        assert_eq!(server.opened.load(Ordering::SeqCst), 1);
        assert!(resolver.warm(ticket.clone()).is_none(), "the pool is already warm");
        for id in 1..=3 {
            let query = a_query(id);
            let answer = resolver.lookup(&ticket, &query).expect("answer");
            assert!(valid_doh_response(&query, &answer));
        }
        assert_eq!(server.opened.load(Ordering::SeqCst), 1, "warm queries reuse the pooled connection");

        // A policy change cuts the pooled connection and the old ticket.
        gate.suspend(|| {});
        gate.resume();
        wait_for("the stale connection to close", || server.closed.load(Ordering::SeqCst) == 1);
        assert!(resolver.lookup(&ticket, &a_query(4)).is_none());
        let ticket = gate.ticket().unwrap();
        assert!(resolver.lookup(&ticket, &a_query(5)).is_some());
        assert!(resolver.lookup(&ticket, &a_query(6)).is_some());
        assert_eq!(server.opened.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn doh_hedges_past_a_stalled_endpoint_and_skips_a_failed_one() {
        let stalled = FakeDoh::start(DohBehavior::Stall);
        let failed = FakeDoh::start(DohBehavior::Fail);
        let good = FakeDoh::start(DohBehavior::Answer);
        let gate = RelayGate::default();
        let ticket = gate.ticket().unwrap();

        let resolver = FakeDoh::resolver(&[&stalled, &good]);
        let start = Instant::now();
        assert!(resolver.lookup(&ticket, &a_query(1)).is_some());
        let elapsed = start.elapsed();
        assert!(elapsed >= DOH_HEDGE && elapsed < DOH_HEDGE + Duration::from_millis(500), "{elapsed:?}");
        // The answering endpoint goes first next time, without a hedge delay.
        let start = Instant::now();
        assert!(resolver.lookup(&ticket, &a_query(2)).is_some());
        assert!(start.elapsed() < DOH_HEDGE, "{:?}", start.elapsed());

        // A failed preferred endpoint starts the others at once.
        let resolver = FakeDoh::resolver(&[&failed, &good]);
        let start = Instant::now();
        assert!(resolver.lookup(&ticket, &a_query(3)).is_some());
        assert!(start.elapsed() < DOH_HEDGE, "{:?}", start.elapsed());

        // No endpoint answers: the lookup ends at its budget.
        let resolver = FakeDoh::resolver(&[&stalled, &failed]);
        let start = Instant::now();
        assert!(resolver.lookup(&ticket, &a_query(4)).is_none());
        let elapsed = start.elapsed();
        assert!(elapsed >= DOH_BUDGET && elapsed < DOH_BUDGET + Duration::from_millis(500), "{elapsed:?}");
    }
}
