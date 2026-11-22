//! The same fixed HTTPS destinations are used by the guest probe and gateway.

use std::net::Ipv4Addr;

pub const PUBLIC_PROBE_V4: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);
pub const HTTPS_URL: &str = "https://1.1.1.1/cdn-cgi/trace";
// The Cloudflare website serves the same trace on a separate anycast address.
pub const ALTERNATE_PROBE_V4: Ipv4Addr = Ipv4Addr::new(104, 16, 123, 96);
pub const ALTERNATE_HTTPS_URL: &str = "https://www.cloudflare.com/cdn-cgi/trace";
