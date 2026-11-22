//! Dev servers to preview: TCP ports a process of this user listens on,
//! read from `/proc/net/tcp{,6}` and matched to processes through their
//! socket inodes. Only loopback and wildcard listeners are reported (the
//! Mac reaches them through an SSH forward to the guest's localhost), and
//! the desktop's own VNC server is left out.

use std::collections::HashMap;
use std::path::Path;

use crate::model::ListeningPort;

const DESKTOP_VNC: u16 = 5900;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Listener {
    pub address: String,
    pub port: u16,
    pub uid: u32,
    pub inode: u64,
}

fn ipv4(hex: &str) -> Option<String> {
    let value = u32::from_str_radix(hex, 16).ok()?;
    // The kernel prints the address in host (little-endian) order.
    let [a, b, c, d] = value.to_le_bytes();
    Some(format!("{a}.{b}.{c}.{d}"))
}

fn ipv6(hex: &str) -> Option<String> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for word in 0..4 {
        let value = u32::from_str_radix(&hex[word * 8..word * 8 + 8], 16).ok()?;
        bytes[word * 4..word * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    Some(std::net::Ipv6Addr::from(bytes).to_string())
}

/// Listening sockets in one `/proc/net/tcp` or `tcp6` table.
pub(crate) fn parse_table(text: &str, v6: bool) -> Vec<Listener> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.get(3) != Some(&"0A") {
                return None;
            }
            let (addr, port) = fields.get(1)?.split_once(':')?;
            let address = if v6 { ipv6(addr)? } else { ipv4(addr)? };
            Some(Listener {
                address,
                port: u16::from_str_radix(port, 16).ok()?,
                uid: fields.get(7)?.parse().ok()?,
                inode: fields.get(9)?.parse().ok()?,
            })
        })
        .collect()
}

fn reachable(address: &str) -> bool {
    matches!(address, "127.0.0.1" | "0.0.0.0" | "::1" | "::")
        || address.starts_with("127.")
        || address == "::ffff:127.0.0.1"
}

/// socket inode → pid, for the processes this user can inspect.
fn socket_owners(proc: &Path) -> HashMap<u64, u32> {
    let mut owners = HashMap::new();
    let Ok(entries) = std::fs::read_dir(proc) else { return owners };
    for entry in entries.filter_map(std::result::Result::ok) {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else { continue };
        for fd in fds.filter_map(std::result::Result::ok) {
            let Ok(target) = std::fs::read_link(fd.path()) else { continue };
            let target = target.to_string_lossy();
            if let Some(inode) = target.strip_prefix("socket:[").and_then(|rest| rest.strip_suffix(']'))
                && let Ok(inode) = inode.parse()
            {
                owners.insert(inode, pid);
            }
        }
    }
    owners
}

fn process_name(proc: &Path, pid: u32) -> (Option<String>, Option<String>) {
    let dir = proc.join(pid.to_string());
    let name = std::fs::read_to_string(dir.join("comm")).ok().map(|text| text.trim().to_string());
    let command = std::fs::read(dir.join("cmdline")).ok().map(|bytes| {
        let text = String::from_utf8_lossy(&bytes).replace('\0', " ");
        let text = text.trim();
        text.chars().take(240).collect::<String>()
    });
    (name, command.filter(|text| !text.is_empty()))
}

/// This user's previewable listeners, by port.
pub fn listening() -> Vec<ListeningPort> {
    let proc = Path::new("/proc");
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let mut listeners = Vec::new();
    for (file, v6) in [("net/tcp", false), ("net/tcp6", true)] {
        if let Ok(text) = std::fs::read_to_string(proc.join(file)) {
            listeners.extend(parse_table(&text, v6));
        }
    }
    let owners = socket_owners(proc);
    let mut ports: Vec<ListeningPort> = Vec::new();
    for listener in listeners {
        if listener.uid != uid || !reachable(&listener.address) || listener.port == DESKTOP_VNC {
            continue;
        }
        if ports.iter().any(|port| port.port == listener.port) {
            continue;
        }
        let pid = owners.get(&listener.inode).copied();
        let (process, command) = pid.map(|pid| process_name(proc, pid)).unwrap_or((None, None));
        if matches!(process.as_deref(), Some("wayvnc" | "cabind")) {
            continue;
        }
        ports.push(ListeningPort { port: listener.port, address: listener.address, process, command, pid });
    }
    ports.sort_by_key(|port| port.port);
    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_net_tcp() {
        let v4 = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:1538 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0 100 0 0 10 0\n\
   1: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 17 1 0 100 0 0 10 0\n\
   2: 0100007F:1538 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 99 1 0 100 0 0 10 0\n";
        let parsed = parse_table(v4, false);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], Listener { address: "127.0.0.1".into(), port: 5432, uid: 1000, inode: 4242 });
        assert_eq!(parsed[1].address, "0.0.0.0");
        assert_eq!(parsed[1].port, 22);

        let v6 = "  sl  local_address remote_address st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n\
   0: 00000000000000000000000001000000:0BB8 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 555 1 0 100 0 0 10 0\n\
   1: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 556 1 0 100 0 0 10 0\n";
        let parsed = parse_table(v6, true);
        assert_eq!(parsed[0].address, "::1");
        assert_eq!(parsed[0].port, 3000);
        assert_eq!(parsed[1].address, "::");
        assert!(reachable("::1") && reachable("0.0.0.0") && !reachable("10.0.2.15"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_a_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let found = listening();
        assert!(found.iter().any(|entry| entry.port == port && entry.pid == Some(std::process::id())), "{found:?}");
    }
}
