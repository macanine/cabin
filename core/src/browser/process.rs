use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Result, error, profile::Profile};

pub(super) fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}
pub(super) fn status_path() -> PathBuf {
    runtime_dir().join("vmctl-fp-status.json")
}
pub(super) fn status() -> Value {
    fs::read(status_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}))
}

pub(super) struct Status {
    path: PathBuf,
    pub(super) value: Value,
}
impl Status {
    pub(super) fn new(data: &Value) -> Self {
        Self {
            path: status_path(),
            value: json!({"ready":false,"profile_fingerprint":data.get("fingerprint")}),
        }
    }
    pub(super) fn update(&mut self, changes: Value) {
        if let (Some(value), Some(changes)) = (self.value.as_object_mut(), changes.as_object()) {
            value.extend(changes.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        let _ = write_status(&self.path, &self.value);
    }
}
fn write_status(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let mut file = File::create(&tmp)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub(super) fn lock() -> Result<Option<File>> {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/vmctl-{uid}")));
    fs::create_dir_all(&dir)?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("vmctl-chrome-controller.lock"))?;
    // SAFETY: file remains open throughout the lifetime of the lock.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        let e = std::io::Error::last_os_error();
        if matches!(e.kind(), std::io::ErrorKind::WouldBlock) {
            return Ok(None);
        }
        return Err(e.into());
    }
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()?;
    Ok(Some(file))
}

fn chrome_binary() -> Result<PathBuf> {
    for path in [
        "/usr/bin/google-chrome-stable",
        "/usr/bin/google-chrome",
        "/opt/google/chrome/chrome",
    ] {
        if Path::new(path).exists() {
            return Ok(path.into());
        }
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for name in ["google-chrome-stable", "google-chrome"] {
            let path = dir.join(name);
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    Err(error("Google Chrome is not installed"))
}

fn string_arg(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) if n.as_f64() != Some(0.0) => Some(n.to_string()),
        _ => None,
    }
}

pub(super) fn chrome_args(
    profile: &Profile,
    port: u16,
    user_data: &str,
    verify: bool,
    restore: bool,
) -> Vec<String> {
    let data = &profile.data;
    let cfg = &data["chrome"];
    let mut args = vec![
        format!("--remote-debugging-port={port}"),
        "--remote-allow-origins=*".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-dev-shm-usage".into(),
        "--disable-quic".into(),
        format!("--user-data-dir={user_data}"),
    ];
    if verify {
        args.extend(
            [
                "--headless=new",
                "--no-sandbox",
                "--use-gl=swiftshader",
                "--enable-unsafe-swiftshader",
            ]
            .map(str::to_owned),
        );
        if let Some(v) = string_arg(&cfg["force_device_scale_factor"]) {
            args.push(format!("--force-device-scale-factor={v}"));
        }
    } else {
        let native = profile.native_display();
        if native {
            args.extend(
                [
                    "--ozone-platform=wayland",
                    "--hide-crash-restore-bubble",
                    "--disable-gpu",
                    "--restore-last-session",
                ]
                .map(str::to_owned),
            );
        } else {
            args.push("--ozone-platform=x11".into());
        }
        if !native
            && let Some(v) = string_arg(&cfg["window_size"])
        {
            args.push(format!("--window-size={v}"));
        }
        if let Some(v) = string_arg(&cfg["lang"]) {
            args.push(format!("--lang={v}"));
        }
        if !native
            && let Some(v) = string_arg(&cfg["force_device_scale_factor"])
        {
            args.push(format!("--force-device-scale-factor={v}"));
        }
        if let Some(v) = string_arg(&data["navigator"]["user_agent"]) {
            args.push(format!("--user-agent={v}"));
        }
        if let Some(flags) = cfg["flags"].as_array() {
            args.extend(flags.iter().filter_map(string_arg));
        }
        if restore && !args.iter().any(|arg| arg == "--restore-last-session") {
            args.push("--restore-last-session".into());
        }
    }
    args.push("about:blank".into());
    args
}

#[cfg(target_os = "linux")]
fn terminate_stale_chrome(user_data: &str) {
    let expected = format!("--user-data-dir={user_data}");
    let mut pids = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            if pid == std::process::id() as i32 {
                continue;
            }
            let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            let mut args = cmdline.split(|b| *b == 0);
            let program = args.next().unwrap_or_default();
            if (program.starts_with(b"/opt/google/chrome/chrome")
                || program.starts_with(b"/usr/bin/google-chrome"))
                && args.any(|arg| arg == expected.as_bytes())
            {
                pids.push(pid);
            }
        }
    }
    for pid in &pids {
        unsafe {
            libc::kill(*pid, libc::SIGTERM);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pids.is_empty() && Instant::now() < deadline {
        pids.retain(|pid| Path::new(&format!("/proc/{pid}")).exists());
        if !pids.is_empty() {
            sleep(Duration::from_millis(100));
        }
    }
    for pid in pids {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn terminate_stale_chrome(_user_data: &str) {}

pub(super) struct Chrome {
    pub(super) child: Child,
    pub(super) port: u16,
    pub(super) temp_dir: Option<PathBuf>,
}
impl Drop for Chrome {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if self.child.try_wait().ok().flatten().is_some() {
                    break;
                }
                sleep(Duration::from_millis(50));
            }
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        if let Some(path) = &self.temp_dir {
            let _ = fs::remove_dir_all(path);
        }
    }
}

pub(super) fn start(profile: &Profile, verify: bool, restore: bool) -> Result<Chrome> {
    let port = if verify {
        std::net::TcpListener::bind(("127.0.0.1", 0))?
            .local_addr()?
            .port()
    } else {
        profile.port()?
    };
    let temp_dir =
        verify.then(|| PathBuf::from(format!("/tmp/vmctl-fp-verify-{}", std::process::id())));
    let user_data = temp_dir
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| {
            profile.data["user_data_dir"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            format!(
                "{}/.config/google-chrome",
                std::env::var("HOME").unwrap_or_default()
            )
        });
    if !verify {
        terminate_stale_chrome(&user_data);
    }
    let log_path = if verify {
        PathBuf::from(format!("/tmp/vmctl-fp-chrome-{}.log", std::process::id()))
    } else {
        PathBuf::from("/tmp/vmctl-fp-chrome.log")
    };
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let child = Command::new(chrome_binary()?)
        .args(chrome_args(profile, port, &user_data, verify, restore))
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()?;
    Ok(Chrome {
        child,
        port,
        temp_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_wayland_and_x11_flags_follow_display_and_restore() {
        let mut profile = Profile {
            data: json!({"native_display":{"enabled":true},"chrome":{"window_size":"800,600","force_device_scale_factor":2.0,"lang":"en-US"},"navigator":{"user_agent":"test"}}),
            source: String::new(),
        };
        let a = chrome_args(&profile, 9222, "/tmp/profile", false, false);
        assert!(a.contains(&"--ozone-platform=wayland".into()));
        assert!(a.contains(&"--restore-last-session".into()));
        assert!(!a.iter().any(
            |v| v.starts_with("--window-size") || v.starts_with("--force-device-scale-factor")
        ));
        profile.data["native_display"]["enabled"] = json!(false);
        let a = chrome_args(&profile, 9222, "/tmp/profile", false, true);
        assert!(a.contains(&"--ozone-platform=x11".into()));
        assert!(a.contains(&"--window-size=800,600".into()));
        assert!(a.contains(&"--restore-last-session".into()));
        let a = chrome_args(&profile, 9222, "/tmp/profile", true, false);
        assert!(a.contains(&"--headless=new".into()));
        assert!(!a.iter().any(|v| v.starts_with("--ozone-platform")));
    }
    #[test]
    fn status_write_replaces_old_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status");
        write_status(&path, &json!({"ready":false})).unwrap();
        write_status(&path, &json!({"ready":true,"pid":12})).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap(),
            json!({"ready":true,"pid":12})
        );
    }
}
