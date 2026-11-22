//! HTTP downloads from inside the VM. They need no proxy settings: every
//! connection the VM makes already goes through the Mac's gateway and route.

use std::path::Path;
use std::time::Duration;

use crate::context::Context;
use crate::error::{Error, Result};
use crate::ops::StepCtx;

/// HTTP client for mirrors.
pub(crate) fn http(_ctx: &Context, timeout: Duration) -> Result<ureq::Agent> {
    Ok(ureq::AgentBuilder::new()
        .resolver(crate::ipv4::resolve)
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(timeout)
        .user_agent(concat!("cabin/", env!("CARGO_PKG_VERSION")))
        .build())
}

pub(crate) fn http_error(what: &str, error: ureq::Error) -> Error {
    match error {
        ureq::Error::Status(code, response) => {
            Error::Unavailable(format!("{what}: HTTP {code} {}", response.status_text()))
        }
        ureq::Error::Transport(transport) => Error::Network(format!("{what}: {transport}")),
    }
}

/// GET a small text resource (index.json, SHASUMS256.txt).
pub(crate) fn get_text(ctx: &Context, url: &str) -> Result<String> {
    http(ctx, Duration::from_secs(60))?
        .get(url)
        .call()
        .map_err(|error| http_error(&format!("Could not load {url}"), error))?
        .into_string()
        .map_err(|error| Error::Network(format!("Could not read {url}: {error}")))
}

/// Stream `url` into `target`, logging every 10%.
pub(crate) fn download_to(
    ctx: &Context,
    url: &str,
    target: &Path,
    step: &StepCtx<'_>,
) -> Result<u64> {
    use std::io::{Read, Write};
    let response = http(ctx, Duration::from_secs(60))?
        .get(url)
        .call()
        .map_err(|error| http_error(&format!("Could not download {url}"), error))?;
    let total: Option<u64> = response
        .header("Content-Length")
        .and_then(|v| v.parse().ok());
    let mut file = std::fs::File::create(target).map_err(|e| Error::path("create", target, e))?;
    let mut reader = response.into_reader();
    let mut buffer = vec![0u8; 256 * 1024];
    let started = std::time::Instant::now();
    let (mut done, mut next) = (0u64, 10u64);
    loop {
        if step.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| Error::Network(format!("Download interrupted: {error}")))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|e| Error::path("write", target, e))?;
        done += read as u64;
        if let Some(total) = total.filter(|t| *t > 0) {
            let percent = done * 100 / total;
            if percent >= next {
                let speed = done as f64 / started.elapsed().as_secs_f64().max(0.001) / 1e6;
                step.log(format!(
                    "{percent}% · {:.1} of {:.1} MB · {speed:.1} MB/s",
                    done as f64 / 1e6,
                    total as f64 / 1e6
                ));
                next = (percent / 10 + 1) * 10;
            }
        }
    }
    if total.is_some_and(|t| done < t) {
        return Err(Error::Network(format!(
            "Download ended early at {done} bytes"
        )));
    }
    Ok(done)
}
