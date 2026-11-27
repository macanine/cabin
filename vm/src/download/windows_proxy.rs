use std::ffi::c_void;
use std::io;
use std::ptr::null;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::Networking::WinHttp::*;

use super::manual_proxy;

struct Session(*mut c_void);

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: The session owns a live WinHTTP handle.
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn take_string(ptr: *mut u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: WinHTTP returns a NUL-terminated UTF-16 allocation owned by the caller.
    unsafe {
        let mut length = 0;
        while *ptr.add(length) != 0 {
            length += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, length));
        GlobalFree(ptr.cast());
        text
    }
}

fn auto_proxy(
    url: &url::Url,
    pac: &str,
    detect: bool,
    timeout: Duration,
) -> io::Result<Option<String>> {
    // SAFETY: Null strings select the default agent and disable a static proxy.
    let session = unsafe { WinHttpOpen(null(), WINHTTP_ACCESS_TYPE_NO_PROXY, null(), null(), 0) };
    if session.is_null() {
        return Err(io::Error::last_os_error());
    }
    let session = Session(session);
    let millis = i32::try_from(timeout.as_millis())
        .unwrap_or(i32::MAX)
        .max(1);
    // SAFETY: Session and timeout values are valid for this call.
    if unsafe { WinHttpSetTimeouts(session.0, millis, millis, millis, millis) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let target: Vec<_> = url.as_str().encode_utf16().chain([0]).collect();
    let pac_url: Vec<_> = pac.encode_utf16().chain([0]).collect();
    let mut options = WINHTTP_AUTOPROXY_OPTIONS::default();
    if !pac.is_empty() {
        options.dwFlags |= WINHTTP_AUTOPROXY_CONFIG_URL;
        options.lpszAutoConfigUrl = pac_url.as_ptr();
    }
    if detect {
        options.dwFlags |= WINHTTP_AUTOPROXY_AUTO_DETECT;
        options.dwAutoDetectFlags = WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A;
    }
    let mut proxy = WINHTTP_PROXY_INFO::default();
    // SAFETY: The session, UTF-16 strings and both WinHTTP structs stay live.
    let success =
        unsafe { WinHttpGetProxyForUrl(session.0, target.as_ptr(), &mut options, &mut proxy) } != 0;
    let error = (!success).then(io::Error::last_os_error);
    let servers = take_string(proxy.lpszProxy);
    let bypass = take_string(proxy.lpszProxyBypass);
    if let Some(error) = error {
        return Err(error);
    }
    if proxy.dwAccessType == WINHTTP_ACCESS_TYPE_NO_PROXY {
        return Ok(None);
    }
    if proxy.dwAccessType == WINHTTP_ACCESS_TYPE_NAMED_PROXY && !servers.trim().is_empty() {
        return if servers.trim().eq_ignore_ascii_case("DIRECT") {
            Ok(None)
        } else {
            Ok(manual_proxy(url, &servers, &bypass))
        };
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "unexpected PAC result",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn winhttp_evaluates_proxy_auto_config_for_the_target() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let pac = format!("http://{}/proxy.pac", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(8);
            while Instant::now() < until {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut reader = BufReader::new(&stream);
                        let mut line = String::new();
                        while reader.read_line(&mut line).unwrap() > 0 && !line.trim().is_empty() {
                            line.clear();
                        }
                        let body = "function FindProxyForURL(url, host) { return host == 'mirror.example.test' ? 'PROXY 127.0.0.1:8888' : 'DIRECT'; }";
                        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                        return;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("PAC listener failed: {error}"),
                }
            }
        });
        let url = url::Url::parse("https://mirror.example.test/image").unwrap();
        assert_eq!(
            auto_proxy(&url, &pac, false, Duration::from_secs(3))
                .unwrap()
                .as_deref(),
            Some("127.0.0.1:8888")
        );
        server.join().unwrap();
    }
}

#[derive(Default)]
struct Settings {
    detect: bool,
    pac: String,
    servers: String,
    bypass: String,
}

fn settings() -> io::Result<Settings> {
    let mut config = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
    // SAFETY: WinHTTP writes to the provided struct and allocates its strings.
    let success = unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut config) } != 0;
    let error = (!success).then(io::Error::last_os_error);
    let settings = Settings {
        detect: config.fAutoDetect != 0,
        pac: take_string(config.lpszAutoConfigUrl),
        servers: take_string(config.lpszProxy),
        bypass: take_string(config.lpszProxyBypass),
    };
    if let Some(error) = error {
        // User profiles without saved Internet Settings have no system proxy.
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(Settings::default())
        } else {
            Err(error)
        };
    }
    Ok(settings)
}

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

struct Slot;

impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(super) fn resolve(url: &url::Url, timeout: Duration) -> io::Result<Option<String>> {
    let started = Instant::now();
    let settings = settings().map_err(proxy_error)?;
    let manual = manual_proxy(url, &settings.servers, &settings.bypass);
    if settings.pac.is_empty() && !settings.detect {
        return Ok(manual);
    }
    let explicit_pac = !settings.pac.is_empty();
    let optional = !explicit_pac || !settings.servers.trim().is_empty();
    // Leave time for the connection when automatic discovery has a fallback.
    let budget = if optional { timeout / 2 } else { timeout };
    match resolve_auto(
        url.clone(),
        settings,
        budget.saturating_sub(started.elapsed()),
    ) {
        Err(error) if optional => {
            if explicit_pac {
                cabin_diagnostics::event(
                    cabin_diagnostics::Level::Warn,
                    "download",
                    "windows_pac_fallback",
                    format_args!("{error}"),
                );
            }
            Ok(manual)
        }
        result => result,
    }
}

fn proxy_error(error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("Windows system proxy resolution failed: {error}"),
    )
}

fn resolve_auto(
    url: url::Url,
    settings: Settings,
    timeout: Duration,
) -> io::Result<Option<String>> {
    let started = Instant::now();
    let timed_out = || {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "Windows system proxy resolution timed out",
        )
    };
    if timeout.is_zero() {
        return Err(timed_out());
    }
    // WinHTTP can outlive the caller's deadline in DHCP/DNS or a PAC script.
    // Keep its slot occupied until the OS call actually returns.
    ACTIVE
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
            (active < 4).then_some(active + 1)
        })
        .map_err(|_| io::Error::other("Windows system proxy resolver is busy"))?;
    let slot = Slot;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("windows-system-proxy".into())
        .spawn(move || {
            let _slot = slot;
            let _ = send.send(auto_proxy(&url, &settings.pac, settings.detect, timeout));
        })
        .map_err(|error| {
            io::Error::other(format!(
                "could not start Windows system proxy resolution: {error}"
            ))
        })?;
    match receive.recv_timeout(timeout.saturating_sub(started.elapsed())) {
        Ok(result) => result.map_err(proxy_error),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(timed_out()),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("Windows system proxy resolver stopped"))
        }
    }
}
