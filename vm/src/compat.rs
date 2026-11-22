//! Host-platform differences beyond QEMU: process control, socket pairs,
//! polling, entropy, storage and time. Unix keeps the existing libc paths;
//! Windows maps each helper onto the native API with matching behavior.

#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

#[cfg(unix)]
pub type WakeSocket = std::os::unix::net::UnixDatagram;
/// A connected datagram socket from [`wake_pair`] / [`packet_pair`].
#[cfg(windows)]
pub type WakeSocket = std::net::UdpSocket;

/// Connected datagram pair for cross-thread wake-ups (never blocks the sender).
pub fn wake_pair() -> io::Result<(WakeSocket, WakeSocket)> {
    #[cfg(unix)]
    {
        let (a, b) = std::os::unix::net::UnixDatagram::pair()?;
        a.set_nonblocking(true)?;
        b.set_nonblocking(true)?;
        Ok((a, b))
    }
    #[cfg(windows)]
    {
        pair_loopback_udp()
    }
}

/// Connected datagram pair carrying whole IP packets between the gateway and
/// the proxy stack. Unix uses a socketpair; Windows two loopback UDP sockets.
pub fn packet_pair() -> io::Result<(WakeSocket, WakeSocket)> {
    #[cfg(unix)]
    {
        let (a, b) = std::os::unix::net::UnixDatagram::pair()?;
        for socket in [&a, &b] {
            // macOS defaults to tiny Unix datagram buffers.
            let size: libc::c_int = 4 * 1024 * 1024;
            for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
                unsafe {
                    libc::setsockopt(
                        socket.as_raw_fd(),
                        libc::SOL_SOCKET,
                        option,
                        (&size as *const libc::c_int).cast(),
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                    );
                }
            }
        }
        a.set_nonblocking(true)?;
        b.set_nonblocking(true)?;
        Ok((a, b))
    }
    #[cfg(windows)]
    {
        pair_loopback_udp()
    }
}

#[cfg(windows)]
fn pair_loopback_udp() -> io::Result<(std::net::UdpSocket, std::net::UdpSocket)> {
    use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
    let first = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    let second = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    let first_addr: SocketAddr = first.local_addr()?;
    let second_addr: SocketAddr = second.local_addr()?;
    first.connect(second_addr)?;
    second.connect(first_addr)?;
    first.set_nonblocking(true)?;
    second.set_nonblocking(true)?;
    Ok((first, second))
}

// ---- readiness polling ----------------------------------------------------

/// Handle used by [`poll`]: a raw fd on Unix, a SOCKET on Windows.
#[cfg(unix)]
pub type PollSock = std::os::fd::RawFd;
#[cfg(windows)]
pub type PollSock = usize;

#[cfg(unix)]
pub const POLLIN: i16 = libc::POLLIN as i16;
#[cfg(unix)]
pub const POLLOUT: i16 = libc::POLLOUT as i16;
#[cfg(windows)]
pub const POLLIN: i16 = 0x0300; // POLLRDNORM | POLLRDBAND
#[cfg(windows)]
pub const POLLOUT: i16 = 0x0010; // POLLWRNORM

/// One polled socket. `events` requests readiness; `revents` reports it.
pub struct PollItem {
    pub sock: PollSock,
    pub events: i16,
    pub revents: i16,
}

/// Anything with an OS poll handle: fds on Unix, sockets on Windows.
pub trait Pollable {
    fn poll_sock(&self) -> PollSock;
}

#[cfg(unix)]
impl<T: std::os::fd::AsRawFd + ?Sized> Pollable for T {
    fn poll_sock(&self) -> PollSock {
        self.as_raw_fd()
    }
}

#[cfg(windows)]
impl<T: std::os::windows::io::AsRawSocket + ?Sized> Pollable for T {
    fn poll_sock(&self) -> PollSock {
        self.as_raw_socket() as usize
    }
}

/// Wait up to `timeout_ms` (-1: forever) for readiness. Returns the number of
/// ready entries; their `revents` carry the result (0 when not ready).
pub fn poll(items: &mut [PollItem], timeout_ms: i32) -> io::Result<usize> {
    #[cfg(unix)]
    {
        let mut fds: Vec<libc::pollfd> = items
            .iter()
            .map(|item| libc::pollfd {
                fd: item.sock,
                events: item.events,
                revents: item.revents,
            })
            .collect();
        let count = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if count < 0 {
            let error = io::Error::last_os_error();
            for (item, fd) in items.iter_mut().zip(fds.iter()) {
                item.revents = fd.revents;
            }
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(0);
            }
            return Err(error);
        }
        for (item, fd) in items.iter_mut().zip(fds.iter()) {
            item.revents = fd.revents;
        }
        Ok(count.max(0) as usize)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Networking::WinSock::{WSAPOLLFD, WSAPoll};
        let mut fds: Vec<WSAPOLLFD> = items
            .iter()
            .map(|item| WSAPOLLFD {
                fd: item.sock,
                events: item.events,
                revents: 0,
            })
            .collect();
        let count = unsafe { WSAPoll(fds.as_mut_ptr(), fds.len() as u32, timeout_ms) };
        if count < 0 {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        for (item, fd) in items.iter_mut().zip(fds.iter()) {
            item.revents = fd.revents;
        }
        Ok(count.max(0) as usize)
    }
}

/// A [`PollItem`] requesting POLLIN for a pollable socket.
pub fn poll_in(sock: &impl Pollable) -> PollItem {
    PollItem {
        sock: sock.poll_sock(),
        events: POLLIN,
        revents: 0,
    }
}

// ---- processes ------------------------------------------------------------

/// Signal numbers kept for the existing call sites. On Windows both map to
/// process termination; the graceful shutdown paths run before either.
pub mod signal {
    #[cfg(unix)]
    pub const SIGTERM: i32 = libc::SIGTERM;
    #[cfg(unix)]
    pub const SIGKILL: i32 = libc::SIGKILL;
    #[cfg(windows)]
    pub const SIGTERM: i32 = 15;
    #[cfg(windows)]
    pub const SIGKILL: i32 = 9;
}

/// True when `pid` names a live process of any user (EPERM counts as alive).
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    pid > 1 && unsafe { libc::kill(pid, 0) == 0 }
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid <= 1 {
        return false;
    }
    let handle: HANDLE = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut code = 0u32;
    let alive =
        unsafe { GetExitCodeProcess(handle, &mut code) != 0 } && code == 259 /* STILL_ACTIVE */;
    unsafe { CloseHandle(handle) };
    alive
}

/// Send `signal` to `pid`. On Windows any signal terminates the process.
pub fn send_signal(pid: u32, signal: i32) {
    #[cfg(unix)]
    {
        if let Ok(pid) = libc::pid_t::try_from(pid)
            && pid > 1
        {
            unsafe { libc::kill(pid, signal) };
        }
    }
    #[cfg(windows)]
    {
        let _ = signal;
        terminate_process(pid, false);
    }
}

/// Kill a process after its graceful window: SIGTERM then SIGKILL on Unix,
/// termination on Windows.
pub fn terminate_process(pid: u32, graceful: bool) {
    #[cfg(unix)]
    {
        send_signal(
            pid,
            if graceful { signal::SIGTERM } else { signal::SIGKILL },
        );
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };
        let _ = graceful;
        if pid <= 1 {
            return;
        }
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if !handle.is_null() {
            unsafe {
                TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }
}

// ---- entropy --------------------------------------------------------------

/// Fill `buffer` with unpredictable bytes. Falls back to a weak source only
/// when the platform one fails (same behavior as the old /dev/urandom path).
pub fn random_bytes(buffer: &mut [u8]) {
    #[cfg(unix)]
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        if std::io::Read::read_exact(&mut file, buffer).is_ok() {
            return;
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{
            BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        };
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status == 0 {
            return;
        }
    }
    let mut seed = (std::process::id() as u64) << 32
        ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
    for byte in buffer.iter_mut() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *byte = (seed >> 33) as u8;
    }
}

// ---- filesystem -----------------------------------------------------------

/// `(total, available)` bytes on `path`'s volume (None when unavailable).
pub fn disk_volume(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        let c_path =
            std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap_or_default();
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: statvfs initializes the supplied buffer on success.
        if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
            return None;
        }
        let bytes = |blocks: u128| {
            u64::try_from(blocks.saturating_mul(stat.f_frsize.into())).unwrap_or(u64::MAX)
        };
        Some((bytes(stat.f_blocks.into()), bytes(stat.f_bavail.into())))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut available = 0u64;
        let mut total = 0u64;
        let mut free = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free)
        };
        if ok == 0 {
            return None;
        }
        Some((total, available))
    }
}

/// Free bytes available to this user on `path`'s volume.
pub fn disk_free_bytes(path: &Path) -> u64 {
    disk_volume(path).map(|(_, available)| available).unwrap_or(u64::MAX)
}

// ---- time -----------------------------------------------------------------

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `strftime("%Y-%m-%dT%H:%M:%S+00:00")` for Unix seconds, no libc needed.
pub fn utc_stamp_string(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let secs = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}+00:00",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Local `%Y%m%d-%H%M%S` stamp (UTC fallback for instants far from now).
pub fn local_stamp_string(at: std::time::SystemTime) -> String {
    #[cfg(unix)]
    {
        let secs = at
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as libc::time_t)
            .unwrap_or(0);
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
            return String::new();
        }
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        // System local time covers "now"; older instants fall back to UTC.
        let now = std::time::SystemTime::now();
        let is_now = at
            .duration_since(now)
            .map(|d| d <= Duration::from_secs(2))
            .unwrap_or(false)
            || now
                .duration_since(at)
                .map(|d| d <= Duration::from_secs(2))
                .unwrap_or(false);
        if is_now {
            let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
            unsafe { GetLocalTime(&mut st) };
            return format!(
                "{:04}{:02}{:02}-{:02}{:02}{:02}",
                st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
            );
        }
        let secs = at
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let days = secs.div_euclid(86_400);
        let rest = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        format!(
            "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
            rest / 3600,
            (rest % 3600) / 60,
            rest % 60
        )
    }
}

// ---- tool lookup ----------------------------------------------------------

/// Locate an external tool. On Unix it must be executable; on Windows the
/// `.exe` suffix completes the name.
pub fn find_tool(name: &str) -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        crate::util::which(name)
    }
    #[cfg(windows)]
    {
        if name.contains(['/', '\\']) {
            let path = std::path::PathBuf::from(name);
            return if path.is_file() { Some(path) } else { None };
        }
        let path = std::env::var_os("PATH")?;
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(format!("{name}.exe"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }
}

/// `curl` invocation: the absolute macOS/Linux tool, or the Windows one from
/// `PATH` (Windows ships curl.exe in System32).
pub fn curl_binary() -> &'static str {
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        "/usr/bin/curl"
    } else {
        "curl"
    }
}

/// Stop spawned console tools (ssh, QEMU, tar, …) from flashing windows.
pub fn no_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

/// The user's home directory (HOME on Unix, USERPROFILE on Windows).
pub fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| Path::new(&home).to_path_buf())
        .filter(|path| !path.as_os_str().is_empty())
}

// ---- network adapters (Windows) -------------------------------------------

#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct AdapterInfo {
    pub name: String,
    pub label: String,
    pub if_index: u32,
    pub if_type: u32,
    pub oper_up: bool,
    pub ipv4: Vec<(std::net::Ipv4Addr, Option<std::net::Ipv4Addr>)>,
}

#[cfg(windows)]
const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
#[cfg(windows)]
const IF_TYPE_PPP: u32 = 23;
#[cfg(windows)]
const IF_TYPE_IEEE80211: u32 = 71;
#[cfg(windows)]
/// IPSec tunnels and other virtual point-to-point transports.
const IF_TYPE_TUNNEL: u32 = 53;
#[cfg(windows)]
/// Propagated virtual bridges (VPN tunnel transports).
const IF_TYPE_PROPVIRTUAL: u32 = 131;

/// Enumerate non-loopback adapters with their IPv4 addresses (Windows).
#[cfg(windows)]
pub fn adapters() -> io::Result<Vec<AdapterInfo>> {
    use std::net::Ipv4Addr;
    use windows_sys::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 0u32;
    // First call reports the required buffer size (error expected).
    let _ = unsafe {
        GetAdaptersAddresses(
            u32::from(windows_sys::Win32::Networking::WinSock::AF_UNSPEC),
            flags,
            std::ptr::null(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u8; size as usize];
    let adapters = buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
    let rc = unsafe {
        GetAdaptersAddresses(
            u32::from(windows_sys::Win32::Networking::WinSock::AF_UNSPEC),
            flags,
            std::ptr::null(),
            adapters,
            &mut size,
        )
    };
    if rc != 0 && rc != ERROR_BUFFER_OVERFLOW {
        return Err(io::Error::last_os_error());
    }

    let mut result = Vec::new();
    let mut current = adapters;
    while !current.is_null() {
        let adapter = unsafe { &*current };
        if adapter.IfType != IF_TYPE_SOFTWARE_LOOPBACK {
            let label = unsafe { wide_string(adapter.FriendlyName) };
            let name = unsafe { byte_string(adapter.AdapterName) };
            let mut ipv4 = Vec::new();
            let mut unicast = adapter.FirstUnicastAddress;
            while !unicast.is_null() {
                let entry = unsafe { &*unicast };
                let sockaddr = entry.Address.lpSockaddr;
                if !sockaddr.is_null() && unsafe { (*sockaddr).sa_family } == AF_INET {
                    let sin = unsafe { &*(sockaddr as *const SOCKADDR_IN) };
                    let octets = unsafe { sin.sin_addr.S_un.S_un_b };
                    let ip = Ipv4Addr::new(octets.s_b1, octets.s_b2, octets.s_b3, octets.s_b4);
                    if !ip.is_unspecified() && !ip.is_loopback() {
                        let prefix = entry.OnLinkPrefixLength.min(32);
                        let mask = if prefix == 0 {
                            None
                        } else {
                            Some(prefix_mask(u32::from(prefix)))
                        };
                        ipv4.push((ip, mask));
                    }
                }
                unicast = entry.Next;
            }
            result.push(AdapterInfo {
                name: if name.is_empty() { label.clone() } else { name },
                label,
                if_index: unsafe { adapter.Anonymous1.Anonymous.IfIndex },
                if_type: adapter.IfType,
                oper_up: adapter.OperStatus == IfOperStatusUp,
                ipv4,
            });
        }
        current = adapter.Next;
    }
    Ok(result)
}

#[cfg(windows)]
fn prefix_mask(prefix: u32) -> std::net::Ipv4Addr {
    let bits = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
    std::net::Ipv4Addr::from(bits.to_be())
}

#[cfg(windows)]
unsafe fn wide_string(pointer: *const u16) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let mut length = 0usize;
    loop {
        // SAFETY: GetAdaptersAddresses returns NUL-terminated UTF-16 buffers.
        if unsafe { *pointer.add(length) } == 0 {
            break;
        }
        length += 1;
    }
    let slice = unsafe { std::slice::from_raw_parts(pointer, length) };
    String::from_utf16_lossy(slice)
}

#[cfg(windows)]
unsafe fn byte_string(pointer: *const u8) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let mut length = 0usize;
    loop {
        // SAFETY: AdapterName is a NUL-terminated ANSI string.
        if unsafe { *pointer.add(length) } == 0 {
            break;
        }
        length += 1;
    }
    let slice = unsafe { std::slice::from_raw_parts(pointer, length) };
    String::from_utf8_lossy(slice).into_owned()
}

/// Whether an address belongs to one of this host's adapters (Windows).
#[cfg(windows)]
pub fn is_host_address_windows(ip: std::net::Ipv4Addr) -> bool {
    adapters()
        .unwrap_or_default()
        .iter()
        .any(|adapter| adapter.ipv4.iter().any(|(address, _)| *address == ip))
}

/// The IPv4 address of a named adapter, or the best default one.
#[cfg(windows)]
pub fn adapter_ip(name: &str) -> Option<String> {
    let adapters = adapters().ok()?;
    let named = if name.is_empty() {
        None
    } else {
        adapters
            .iter()
            .find(|adapter| adapter.name == name || adapter.label == name)
            .filter(|adapter| adapter.oper_up && !adapter.ipv4.is_empty())
    };
    if let Some(adapter) = named {
        return Some(adapter.ipv4[0].0.to_string());
    }
    adapters
        .iter()
        .filter(|adapter| adapter.oper_up && !adapter.ipv4.is_empty())
        .min_by_key(|adapter| adapter.if_index)
        .map(|adapter| adapter.ipv4[0].0.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_stamps_match_the_libc_format() {
        // 2026-11-16T03:46:45+00:00
        assert_eq!(utc_stamp_string(1_794_800_805), "2026-11-16T03:46:45+00:00");
        assert_eq!(utc_stamp_string(0), "1970-01-01T00:00:00+00:00");
        // Leap year and month boundaries.
        assert_eq!(utc_stamp_string(951_782_400), "2000-02-29T00:00:00+00:00");
        assert_eq!(utc_stamp_string(951_868_800), "2000-03-01T00:00:00+00:00");
    }

    #[test]
    fn wake_pair_transports_datagrams() {
        let (a, b) = wake_pair().unwrap();
        a.send(&[7u8; 5]).unwrap();
        let mut buffer = [0u8; 8];
        let read = b.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..read], &[7u8; 5]);
    }

    #[test]
    fn random_bytes_are_filling() {
        let mut buffer = [0u8; 64];
        random_bytes(&mut buffer);
        let mut unique = std::collections::HashSet::new();
        buffer.iter().for_each(|byte| {
            unique.insert(byte);
        });
        assert!(unique.len() > 10, "entropy source produced repeating bytes");
    }

    #[cfg(windows)]
    #[test]
    fn adapters_list_at_least_one_usable_link() {
        let list = adapters().unwrap();
        assert!(list.iter().any(|adapter| adapter.oper_up), "{list:?}");
    }
}
