//! Testing ARM64 package mirrors (Ubuntu `ubuntu-ports`) for the
//! chosen OS before an install. Candidates come from `os::Os::mirrors`.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::download::{HttpOptions, http_error, is_html, request};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorResult {
    pub uri: String,
    pub host: String,
    /// ok | noArm64 | unreachable
    pub status: String,
    pub latency_ms: Option<u32>,
    /// The archive the image uses when no mirror is chosen.
    #[serde(default)]
    pub default: bool,
    /// Measured download speed of the arm64 package index.
    pub speed_kbps: Option<u32>,
    pub detail: String,
}

/// Status, time until the response started, and body speed (when read).
struct Fetch {
    status: u16,
    first_byte_ms: u32,
    bytes_per_sec: Option<f64>,
}

/// GET the release or index using the installer's host proxy rules.
fn fetch(url: &str, max_secs: u64, read_body: bool) -> Result<Fetch, String> {
    let started = Instant::now();
    let response = match request(
        "GET",
        url,
        None,
        &HttpOptions {
            read_timeout: Duration::from_secs(8),
            timeout: Some(Duration::from_secs(max_secs)),
        },
    ) {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => return Err(http_error(&error)),
    };
    if response.status() == 200 && is_html(&response) {
        return Err("download received an HTML page instead of the package index; check for a captive portal".into());
    }
    let first_byte_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    let status = response.status();
    let mut bytes_per_sec = None;
    if read_body && status == 200 {
        let body_started = Instant::now();
        let read = std::io::copy(&mut response.into_reader(), &mut std::io::sink())
            .map_err(|error| format!("could not download the package index: {error}"))?;
        let secs = body_started.elapsed().as_secs_f64().max(0.001);
        bytes_per_sec = Some(read as f64 / secs);
    }
    Ok(Fetch {
        status,
        first_byte_ms,
        bytes_per_sec,
    })
}

pub fn test_one(os: &crate::os::Os, uri: &str) -> MirrorResult {
    let uri = uri.trim().trim_end_matches('/').to_string();
    let host = url::Url::parse(&uri)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| uri.clone());
    let result =
        |status: &str, latency: Option<u32>, speed: Option<u32>, detail: String| MirrorResult {
            uri: uri.clone(),
            host: host.clone(),
            status: status.into(),
            latency_ms: latency,
            default: false,
            speed_kbps: speed,
            detail,
        };
    let release = match fetch(&format!("{uri}/dists/{}/Release", os.codename), 15, false) {
        Ok(release) => release,
        Err(detail) => return result("unreachable", None, None, detail),
    };
    let latency = Some(release.first_byte_ms);
    match release.status {
        200 => {}
        404 => {
            return result(
                "noArm64",
                latency,
                None,
                format!("No {} packages here", os.name),
            );
        }
        other => return result("unreachable", latency, None, format!("HTTP {other}")),
    }
    let guest = crate::arch::GuestArch::current();
    let arch = guest.debian();
    let index = format!(
        "{uri}/dists/{}/main/binary-{arch}/Packages.gz",
        os.mirror_index_suite()
    );
    match fetch(&index, 30, true) {
        Ok(index) if index.status == 200 => {
            let speed = index
                .bytes_per_sec
                .map(|bytes| (bytes / 1024.0).round() as u32);
            result(
                "ok",
                latency,
                speed,
                format!(
                    "{} packages available{}",
                    guest.label(),
                    speed.map(|s| format!(", {s} KB/s")).unwrap_or_default()
                ),
            )
        }
        Ok(index) => result(
            "noArm64",
            latency,
            None,
            format!("Release found but no {arch} index (HTTP {})", index.status),
        ),
        Err(detail) => result("unreachable", latency, None, detail),
    }
}

/// Test mirrors in parallel; usable ones first, fastest first.
pub fn test_mirrors(os: &crate::os::Os, uris: &[String]) -> Vec<MirrorResult> {
    let mut results: Vec<MirrorResult> = std::thread::scope(|scope| {
        let handles: Vec<_> = uris
            .iter()
            .map(|uri| scope.spawn(move || test_one(os, uri)))
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let default = os.mirrors().first().copied().unwrap_or("");
    for result in &mut results {
        result.default = result.uri == default;
    }
    results.sort_by_key(|r| {
        (
            r.status != "ok",
            std::cmp::Reverse(r.speed_kbps.unwrap_or(0)),
        )
    });
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreachable_mirror_is_reported_not_fatal() {
        let result = test_one(&crate::os::RELEASES[0], "http://127.0.0.1:1/ubuntu-ports");
        assert_eq!(result.status, "unreachable");
        assert_eq!(result.host, "127.0.0.1");
    }

    #[test]
    fn captive_portal_is_not_ranked_as_a_package_mirror() {
        use std::io::{BufRead, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for mut stream in listener.incoming().take(2).map(Result::unwrap) {
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&stream);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let content_type = if first.contains("/Release ") {
                    "text/plain"
                } else {
                    "text/html"
                };
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() <= 2 {
                        break;
                    }
                }
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
        });
        let result = test_one(&crate::os::RELEASES[0], &format!("http://{address}"));
        server.join().unwrap();
        assert_eq!(result.status, "unreachable");
        assert!(result.detail.contains("captive portal"), "{result:?}");
    }

    #[test]
    #[ignore = "requires outbound network"]
    fn real_mirrors_are_ranked() {
        for os in crate::os::RELEASES {
            let uris: Vec<String> = os.mirrors().iter().map(|s| s.to_string()).collect();
            let results = test_mirrors(&os, &uris);
            for r in &results {
                eprintln!(
                    "{} {} {} {:?} {:?}",
                    os.id, r.host, r.status, r.latency_ms, r.speed_kbps
                );
            }
            assert!(results.iter().any(|r| r.status == "ok"), "{}", os.id);
        }
    }
}
