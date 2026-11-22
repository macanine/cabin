# IP detection

`cabin-ip-detection` is a Rust library for observing a public exit address and
classifying an IP address. The two lookups report their own sources. The desktop app
observes the exit through the VM gateway and caches classification by address.
[Gateway architecture](../PLAN.md#network-routing-and-recovery)

## Usage

Add the local crate in the caller’s `Cargo.toml`.

```toml
cabin-ip-detection = { path = "../ip-detection" }
```

```rust
use cabin_ip_detection::{Detector, Service};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let detector = Detector::builder()
        .proxy_url("http://127.0.0.1:7890")
        .build()?;

    let info = detector.detect()?;
    println!("{} via {}", info.ip, info.source.name());
    println!("Provider: {:?}, ASN: {:?}", info.isp(), info.asn);

    let classification = detector.classify(info.ip);
    println!("Classification: {classification:?}");

    let from_ip_sb = detector.detect_service(Service::IpSb)?;
    println!("IP.SB: {}", from_ip_sb.ip);
    Ok(())
}
```

`services([Service::IpSb, Service::IpWhoIs])` chooses the address providers. `timeout`,
`retries`, and `retry_delay` configure address lookup. `DetectionError.failures` reports
individual service errors and attempt counts. `ipv4_only()` restricts lookups to IPv4.

The default client uses `ALL_PROXY`, then `HTTPS_PROXY`, then `HTTP_PROXY`, with
lowercase forms supported. Use `proxy_url()` to select a specific HTTP, SOCKS4, SOCKS4a, or
SOCKS5 proxy. Explicit proxy URLs accept hostnames and IPv4 addresses. Use `direct()` for a
direct connection. System proxy settings and `NO_PROXY` are outside the client's environment
proxy selection.

## Provider order

**Exit address.** Cloudflare Trace, IP.SB, IPinfo, IPWHOIS, GeoJS, country.is, IPQuery, then
ident.me. The first request starts immediately. Another service starts after a failed result
or an 800 ms wait, with up to three requests in flight. The default search returns the first
valid public address within a six-second deadline.
[FlClash v0.9.4 address sources](https://github.com/chen08209/FlClash/blob/v0.9.4/lib/common/service_probe.dart)

**IP classification.** ident.me, ip-api.com, and IPQuery start together. IPLocate, ipapi.is,
and ProxyCheck follow. An explicit provider classification takes precedence. The library
uses inferred residential classification as a fallback.
[FlClash v0.9.4 classification sources](https://github.com/chen08209/FlClash/blob/v0.9.4/lib/common/ip_quality.dart)

## Verification

From this directory, run `cargo test` for offline tests or `cargo run --example detect` to
query the current exit.
