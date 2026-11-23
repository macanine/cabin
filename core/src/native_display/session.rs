use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::system::{self, CommandSpec, HostSystem, System};
use crate::{Error, Result, fsx};

use super::state::{Output, read_optional};

const CHROME: &str = "/opt/google/chrome/chrome";
const CHROME_BINARY: &str = "/usr/bin/google-chrome-stable";
const LEGACY_CONTROLLER: &str = "/opt/vmctl-fp/controller.py";
const BROWSER_HELPER: &str = "/usr/local/libexec/cabin/cabind";
const CHROME_WRAPPER: &str = "#!/bin/sh\n# Keep one managed Chrome/controller alive if Chrome is closed or crashes.\nexport DISPLAY=\"${DISPLAY:-:0}\"\nwhile :; do\n  /usr/local/libexec/cabin/cabind browser run\n  code=$?\n  [ \"$code\" -eq 75 ] && exit 0\n  sleep 2\ndone\n";
const OLD_CHROME_WRAPPER: &str = "#!/bin/sh\n# Keep one managed Chrome/controller alive if Chrome is closed or crashes.\nexport DISPLAY=\"${DISPLAY:-:0}\"\nwhile :; do\n  /usr/bin/python3 /opt/vmctl-fp/controller.py\n  code=$?\n  [ \"$code\" -eq 75 ] && exit 0\n  sleep 2\ndone\n";
const OLDEST_CHROME_WRAPPER: &str = "#!/bin/sh\n# Keep the managed Chrome/controller alive if Chrome is closed or crashes.\nwhile :; do\n  /usr/bin/python3 /opt/vmctl-fp/controller.py\n  sleep 2\ndone\n";
const WAYVNC: &str = "/usr/bin/wayvnc";
const WAYVNC_SUPERVISOR: &str = "/usr/local/libexec/cabin-wayvnc";
const VNC_ADDRESS: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 5900);
const INTERVAL: Duration = Duration::from_millis(100);

pub(super) trait Session: Send + Sync {
    fn output(&self) -> Result<Output>;
    fn set_output(&self, before: &Output, target: &Output) -> Result<()>;
    fn verify_viewer(&self, output: &Output) -> Result<()>;
    fn chrome(&self) -> Chrome;
    fn restart_chrome(&self, wayland: bool) -> Result<()>;
    fn install(&self, path: &Path, bytes: &[u8], mode: u32) -> Result<()>;
    fn remove(&self, path: &Path) -> Result<()>;
}

#[derive(Default)]
pub(super) struct Chrome {
    pub pid: Option<u32>,
    pub ready: bool,
    pub wayland: bool,
    diagnostic: String,
}

#[derive(Serialize)]
pub(super) struct BrowserStatus {
    chrome_installed: bool,
    chrome_version: Option<String>,
    controller_running: bool,
    chrome_running: bool,
    viewer_ready: bool,
}

pub(super) struct GuestSession {
    pub home: PathBuf,
    pub state: PathBuf,
    uid: u32,
    runtime: PathBuf,
    system: HostSystem,
}

impl GuestSession {
    pub fn new(home: &Path, state: &Path) -> Self {
        let uid = unsafe { libc::getuid() };
        Self {
            home: home.into(),
            state: state.into(),
            uid,
            runtime: PathBuf::from(format!("/run/user/{uid}")),
            system: HostSystem {
                path: std::env::var("PATH")
                    .unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
            },
        }
    }

    fn env(&self) -> Result<Vec<(String, String)>> {
        let not_ready = || Error::Unavailable("The desktop session is not ready.".into());
        let mut sway = Vec::new();
        let mut displays = Vec::new();
        for entry in fs::read_dir(&self.runtime)
            .map_err(|_| not_ready())?
            .flatten()
        {
            if !entry.file_type().is_ok_and(|kind| kind.is_socket()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("sway-ipc.") {
                sway.push(entry.path());
            }
            if name.starts_with("wayland-") {
                displays.push(name);
            }
        }
        displays.sort_unstable();
        let [sway] = sway.as_slice() else {
            return Err(not_ready());
        };
        let display = displays.into_iter().next().ok_or_else(not_ready)?;
        Ok(vec![
            (
                "XDG_RUNTIME_DIR".into(),
                self.runtime.to_string_lossy().into_owned(),
            ),
            ("SWAYSOCK".into(), sway.to_string_lossy().into_owned()),
            ("WAYLAND_DISPLAY".into(), display),
        ])
    }

    fn sway<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let spec = CommandSpec::new(
            ["swaymsg", "-r"]
                .into_iter()
                .map(str::to_owned)
                .chain(args.into_iter().map(|arg| arg.as_ref().to_owned())),
        )
        .env(self.env()?)
        .timeout(Duration::from_secs(10));
        self.run(&spec)
    }

    fn run(&self, spec: &CommandSpec) -> Result<String> {
        self.system.check(spec).map_err(|error| {
            if let Error::Command { message, output } = error {
                return Error::Unavailable(if output.trim().is_empty() {
                    message
                } else {
                    output
                });
            }
            error
        })
    }

    fn processes(&self) -> Vec<Process> {
        let Ok(entries) = fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let pid = entry.file_name().to_str()?.parse().ok()?;
                process(pid, self.uid)
            })
            .collect()
    }

    pub fn browser_status(&self) -> BrowserStatus {
        let processes = self.processes();
        let installed = Path::new(CHROME_BINARY).is_file();
        let version = installed
            .then(|| {
                self.run(
                    &CommandSpec::new([CHROME_BINARY, "--version"]).timeout(Duration::from_secs(5)),
                )
                .ok()
            })
            .flatten()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        BrowserStatus {
            chrome_installed: installed,
            chrome_version: version,
            controller_running: processes.iter().any(Process::controller),
            chrome_running: processes.iter().any(Process::chrome_main),
            viewer_ready: connect_vnc()
                .and_then(|mut connection| rfb_greeting(&mut connection))
                .is_ok(),
        }
    }

    fn spawn(&self, program: &Path, args: &[&str], log: &str, env: &[(&str, &str)]) -> Result<()> {
        fsx::ensure_dir(&self.state)?;
        let path = crate::paths::AppPaths::from_env().logs().join(log);
        let log = cabin_diagnostics::open_capture(&path, cabin_diagnostics::Capture::Replace)?;
        let mut command = Command::new(program);
        command
            .args(args)
            .envs(self.env()?)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        // The guest process must survive the completion of the SSH command.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        command
            .spawn()
            .map_err(|error| Error::path("start", program, error))?;
        Ok(())
    }
}

impl Session for GuestSession {
    fn output(&self) -> Result<Output> {
        read_output(&self.sway(["-t", "get_outputs"])?)
    }

    fn set_output(&self, before: &Output, target: &Output) -> Result<()> {
        let args = output_command(before, target)?;
        if args.is_empty() {
            return Ok(());
        }
        sway_result(&self.sway(args)?)
    }

    fn verify_viewer(&self, output: &Output) -> Result<()> {
        if wait_for_vnc(output, Duration::from_secs(2)).is_ok() {
            return Ok(());
        }
        let processes = self.processes();
        let pids: Vec<_> = processes
            .iter()
            .filter(|process| process.wayvnc())
            .map(|process| process.pid)
            .collect();
        match pids.as_slice() {
            [] => (),
            [pid] => stop_processes(&[*pid])?,
            _ => {
                return Err(Error::Unavailable(
                    "The managed VNC display could not be identified safely.".into(),
                ));
            }
        }
        // Existing installs may still start wayvnc directly from Sway.
        if Path::new(WAYVNC_SUPERVISOR).is_file() {
            if !processes
                .iter()
                .any(|process| process.args.iter().any(|arg| arg == WAYVNC_SUPERVISOR))
            {
                sway_result(&self.sway(["exec", WAYVNC_SUPERVISOR])?)?;
            }
        } else if TcpStream::connect_timeout(&VNC_ADDRESS, INTERVAL).is_err() {
            self.spawn(
                Path::new(WAYVNC),
                &["--max-fps=60", "127.0.0.1", "5900"],
                "wayvnc.log",
                &[],
            )?;
        }
        wait_for_vnc(output, Duration::from_secs(10))
    }

    fn chrome(&self) -> Chrome {
        let status = fs::read(self.runtime.join("vmctl-fp-status.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ChromeRecord>(&bytes).ok())
            .unwrap_or_default();
        let Some(process) = status
            .pid
            .and_then(|pid| process(pid, self.uid))
            .filter(Process::chrome_main)
        else {
            return Chrome {
                diagnostic: format!(
                    "stage={}, error={}",
                    status.stage,
                    status.error.unwrap_or_default()
                ),
                ..Chrome::default()
            };
        };
        let wayland = chrome_uses_wayland(&process.args);
        Chrome {
            pid: Some(process.pid),
            ready: status.ready,
            wayland,
            diagnostic: format!(
                "stage={}, ready={}, wayland={wayland}, pid={}, error={}",
                status.stage,
                status.ready,
                process.pid,
                status.error.unwrap_or_default()
            ),
        }
    }

    fn restart_chrome(&self, wayland: bool) -> Result<()> {
        let wrapper = self.home.join(".config/sway/start-chrome.sh");
        if !Path::new(BROWSER_HELPER).is_file() {
            return Err(Error::Unavailable("The browser controller is not installed.".into()));
        }
        migrate_chrome_wrapper(&wrapper)?;
        let wrapper_name = wrapper.to_string_lossy();
        let old_pid = self.chrome().pid;
        let processes = self.processes();
        let wrappers: Vec<_> = processes
            .iter()
            .filter(|p| p.chrome_wrapper(&wrapper_name))
            .map(|p| p.pid)
            .collect();
        stop_processes(&wrappers)?;
        let controllers: Vec<_> = self
            .processes()
            .iter()
            .filter(|p| p.controller())
            .map(|p| p.pid)
            .collect();
        stop_processes(&controllers)?;
        if let Some(pid) = self.chrome().pid.or(old_pid).filter(|pid| alive(*pid)) {
            let close = CommandSpec::new([BROWSER_HELPER, "browser", "close"])
                .timeout(Duration::from_secs(5));
            if self.run(&close).is_err() || !wait_for_exit(&[pid], Duration::from_secs(5)) {
                stop_processes(&[pid])?;
            }
        }
        fsx::restore(&self.runtime.join("vmctl-fp-status.json"), None)?;
        let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into());
        self.spawn(
            &wrapper,
            &[],
            "chrome-recovery.log",
            &[
                ("CABIN_CHROME_RESTORE", "1"),
                ("XDG_SESSION_TYPE", "wayland"),
                ("DISPLAY", &display),
            ],
        )?;
        let deadline = Instant::now() + Duration::from_secs(80);
        let mut stable = None;
        while Instant::now() < deadline {
            let chrome = self.chrome();
            if let Some(pid) = chrome
                .pid
                .filter(|pid| Some(*pid) != old_pid && chrome.ready && chrome.wayland == wayland)
            {
                match stable {
                    Some((previous, since)) if previous == pid => {
                        if Instant::now().duration_since(since) >= Duration::from_secs(2) {
                            return Ok(());
                        }
                    }
                    _ => stable = Some((pid, Instant::now())),
                }
            } else {
                stable = None;
            }
            sleep(INTERVAL);
        }
        let backend = if wayland { "Wayland" } else { "X11" };
        let log = tail_chrome_log();
        Err(Error::Unavailable(format!(
            "Chrome did not restart with the requested {backend} display backend ({}).{log}",
            self.chrome().diagnostic
        )))
    }

    fn install(&self, path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
        // Keep the root-owned target unchanged until its replacement is ready.
        let id = uuid::Uuid::new_v4().simple().to_string();
        let source = self.state.join(format!("install-{id}"));
        let target = path.with_extension(format!("cabin-{id}"));
        fsx::atomic_write(&source, bytes, Some(0o600))?;
        let root = |args: &[&str]| {
            self.run(&system::privileged(args.iter().copied()).timeout(Duration::from_secs(10)))
        };
        let source_arg = source.to_string_lossy();
        let target = target.to_string_lossy();
        let destination = path.to_string_lossy();
        let result = root(&[
            "install",
            "-m",
            &format!("{mode:04o}"),
            &source_arg,
            &target,
        ])
        .and_then(|_| root(&["mv", "-f", &target, &destination]));
        if result.is_err() {
            let _ = root(&["rm", "-f", &target]);
        }
        let _ = fs::remove_file(&source);
        result.map(|_| ())
    }

    fn remove(&self, path: &Path) -> Result<()> {
        self.run(
            &system::privileged(["rm", "-f", &path.to_string_lossy()])
                .timeout(Duration::from_secs(10)),
        )?;
        Ok(())
    }
}

pub(super) fn verify_output(actual: &Output, target: &Output) -> Result<()> {
    if actual.same_mode(target) && (actual.scale - target.scale).abs() <= 0.02 {
        return Ok(());
    }
    Err(Error::Unavailable(format!(
        "The desktop reports {} at scale {}. Requested {} at scale {}.",
        actual.dimensions(),
        actual.scale,
        target.dimensions(),
        target.scale
    )))
}

fn output_command(before: &Output, target: &Output) -> Result<Vec<String>> {
    target.validate()?;
    if before.name != target.name {
        return Err(Error::Unavailable(
            "The active desktop output changed.".into(),
        ));
    }
    let mut args = vec!["--".into(), "output".into(), target.name.clone()];
    if !before.same_mode(target) {
        args.extend(target.mode_args());
    }
    if (target.scale - before.scale).abs() > 0.000_001 {
        args.extend(["scale".into(), target.scale.to_string()]);
    }
    if args.len() == 3 {
        args.clear();
    }
    Ok(args)
}

#[derive(Deserialize)]
struct SwayOutput {
    active: bool,
    name: String,
    current_mode: Option<Mode>,
    scale: Option<f64>,
}

#[derive(Deserialize)]
struct Mode {
    width: u32,
    height: u32,
}

fn read_output(data: &str) -> Result<Output> {
    let mut active = serde_json::from_str::<Vec<SwayOutput>>(data)?
        .into_iter()
        .filter(|out| out.active);
    if let (
        Some(SwayOutput {
            name,
            scale: Some(scale),
            current_mode: Some(Mode { width, height }),
            ..
        }),
        None,
    ) = (active.next(), active.next())
    {
        let output = Output {
            name,
            width,
            height,
            scale,
        };
        output.validate()?;
        return Ok(output);
    }
    Err(Error::Unavailable(
        "Native scaling needs one active desktop display.".into(),
    ))
}

fn sway_result(data: &str) -> Result<()> {
    #[derive(Deserialize)]
    struct Reply {
        success: bool,
        error: Option<String>,
    }
    let replies: Vec<Reply> = serde_json::from_str(data)?;
    if !replies.is_empty() && replies.iter().all(|reply| reply.success) {
        return Ok(());
    }
    Err(Error::Unavailable(
        replies
            .into_iter()
            .find(|reply| !reply.success)
            .and_then(|reply| reply.error)
            .unwrap_or_else(|| "The desktop rejected this resolution or scale.".into()),
    ))
}

fn wait_for_vnc(output: &Output, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let problem = match vnc_size() {
            Ok((width, height))
                if (u32::from(width), u32::from(height)) == (output.width, output.height) =>
            {
                return Ok(());
            }
            Ok((width, height)) => format!("VNC reports {width}x{height}"),
            Err(error) => error.to_string(),
        };
        if Instant::now() >= deadline {
            return Err(Error::Unavailable(format!(
                "The VNC display did not reach {} ({problem}).",
                output.dimensions()
            )));
        }
        sleep(INTERVAL);
    }
}

fn connect_vnc() -> Result<TcpStream> {
    let timeout = Duration::from_millis(500);
    let connection = TcpStream::connect_timeout(&VNC_ADDRESS, timeout)?;
    connection.set_read_timeout(Some(timeout))?;
    connection.set_write_timeout(Some(timeout))?;
    Ok(connection)
}

fn vnc_size() -> Result<(u16, u16)> {
    rfb_size(&mut connect_vnc()?)
}

fn rfb_greeting(connection: &mut impl Read) -> Result<()> {
    let mut greeting = [0; 12];
    connection.read_exact(&mut greeting)?;
    if !greeting.starts_with(b"RFB ") {
        return Err(Error::Unavailable(
            "The display did not return a VNC greeting.".into(),
        ));
    }
    Ok(())
}

fn rfb_size(connection: &mut (impl Read + Write)) -> Result<(u16, u16)> {
    rfb_greeting(connection)?;
    connection.write_all(b"RFB 003.008\n")?;
    let mut count = [0];
    connection.read_exact(&mut count)?;
    let mut security = vec![0; usize::from(count[0])];
    connection.read_exact(&mut security)?;
    if !security.contains(&1) {
        return Err(Error::Unavailable(
            "The VNC display rejected the local connection.".into(),
        ));
    }
    connection.write_all(&[1])?;
    let mut result = [0; 4];
    connection.read_exact(&mut result)?;
    if result != [0; 4] {
        return Err(Error::Unavailable(
            "The VNC display could not authenticate locally.".into(),
        ));
    }
    // Keep existing viewer connections when getting ServerInit dimensions.
    connection.write_all(&[1])?;
    connection.read_exact(&mut result)?;
    Ok((
        u16::from_be_bytes([result[0], result[1]]),
        u16::from_be_bytes([result[2], result[3]]),
    ))
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ChromeRecord {
    pid: Option<u32>,
    ready: bool,
    stage: String,
    error: Option<String>,
}

struct Process {
    pid: u32,
    args: Vec<String>,
}

fn migrate_chrome_wrapper(wrapper: &Path) -> Result<()> {
    let old = fs::read_to_string(wrapper).unwrap_or_default();
    if old == CHROME_WRAPPER {
        return Ok(());
    }
    if !old.is_empty() && old != OLD_CHROME_WRAPPER && old != OLDEST_CHROME_WRAPPER {
        return Err(Error::Conflict("An unrecognized Chrome wrapper must not be replaced.".into()));
    }
    let temp = wrapper.with_extension("sh.native");
    fsx::atomic_write(&temp, CHROME_WRAPPER.as_bytes(), Some(0o755))?;
    fs::rename(&temp, wrapper)?;
    Ok(())
}

impl Process {
    fn controller(&self) -> bool {
        matches!(self.args.as_slice(), [helper, command, action]
            if helper == BROWSER_HELPER && command == "browser" && action == "run")
            || self.args == ["/usr/bin/python3", LEGACY_CONTROLLER]
    }

    fn chrome_wrapper(&self, wrapper: &str) -> bool {
        matches!(self.args.as_slice(), [shell, script] if shell == "/bin/sh" && script == wrapper)
            || matches!(self.args.as_slice(), [shell, flag, script] if shell == "/bin/sh" && flag == "-c" && script == wrapper)
    }

    fn chrome_main(&self) -> bool {
        self.args
            .first()
            .is_some_and(|first| first.split(' ').next() == Some(CHROME))
            && !self
                .args
                .iter()
                .any(|arg| arg.starts_with("--type=") || arg.contains(" --type="))
    }

    fn wayvnc(&self) -> bool {
        self.args.first().is_some_and(|program| {
            Path::new(program)
                .file_name()
                .is_some_and(|name| name == "wayvnc")
        }) && self.args.iter().any(|arg| arg == "127.0.0.1")
            && self.args.iter().any(|arg| arg == "5900")
    }
}

fn chrome_uses_wayland(args: &[String]) -> bool {
    args.iter()
        .flat_map(|arg| arg.split_ascii_whitespace())
        .any(|arg| arg == "--ozone-platform=wayland")
}

fn process(pid: u32, uid: u32) -> Option<Process> {
    let root = Path::new("/proc").join(pid.to_string());
    if root.metadata().ok()?.uid() != uid {
        return None;
    }
    let args = read_optional(&root.join("cmdline")).ok()??;
    let args = String::from_utf8_lossy(&args)
        .trim_end_matches('\0')
        .split('\0')
        .map(str::to_owned)
        .collect();
    Some(Process { pid, args })
}

fn alive(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, tail)| !tail.starts_with("Z "))
        })
        .unwrap_or(false)
}

fn wait_for_exit(pids: &[u32], timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while pids.iter().copied().any(alive) {
        if Instant::now() >= deadline {
            return false;
        }
        sleep(INTERVAL);
    }
    true
}

fn stop_processes(pids: &[u32]) -> Result<()> {
    for (signal, timeout) in [(libc::SIGTERM, 8), (libc::SIGKILL, 2)] {
        for pid in pids.iter().copied().filter(|pid| alive(*pid)) {
            if unsafe { libc::kill(pid as libc::pid_t, signal) } < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error.into());
                }
            }
        }
        if wait_for_exit(pids, Duration::from_secs(timeout)) {
            return Ok(());
        }
    }
    Err(Error::Unavailable(
        "A managed desktop process did not stop.".into(),
    ))
}

fn tail_chrome_log() -> String {
    let tail = cabin_diagnostics::tail(&crate::browser::chrome_log_path(), 3000, 0);
    if tail.is_empty() { tail } else { format!("\n{tail}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_only_does_not_reapply_an_existing_mode() {
        let before = Output {
            name: "Virtual-1".into(),
            width: 1920,
            height: 1080,
            scale: 1.0,
        };
        let target = Output {
            scale: 2.0,
            ..before.clone()
        };
        assert_eq!(
            output_command(&before, &target).unwrap(),
            ["--", "output", "Virtual-1", "scale", "2"]
        );
        assert!(output_command(&before, &before).unwrap().is_empty());
    }

    #[test]
    fn drm_custom_modes_round_trip_through_apply_restore_and_sway_config() {
        let baseline = Output {
            name: "Virtual-1".into(),
            width: 1920,
            height: 1080,
            scale: 1.0,
        };
        let target = Output {
            width: 1512,
            height: 982,
            scale: 2.0,
            ..baseline.clone()
        };
        assert_eq!(
            output_command(&baseline, &target).unwrap(),
            [
                "--",
                "output",
                "Virtual-1",
                "mode",
                "--custom",
                "1512x982",
                "scale",
                "2"
            ]
        );
        assert_eq!(
            output_command(&target, &baseline).unwrap(),
            [
                "--",
                "output",
                "Virtual-1",
                "mode",
                "--custom",
                "1920x1080",
                "scale",
                "1"
            ]
        );
        let sway = String::from_utf8(
            super::super::state::sway_config("# existing\n", Some(&target)).unwrap(),
        )
        .unwrap();
        assert!(sway.contains("output \"Virtual-1\" mode --custom 1512x982 scale 2\n"));
        assert_eq!(
            super::super::state::sway_config(&sway, None).unwrap(),
            b"# existing\n"
        );
    }

    #[test]
    fn reads_physical_mode_and_keeps_sway_errors() {
        let reply = r#"[{"name":"Virtual-2","active":false},{"name":"Virtual-1","active":true,"current_mode":{"width":1920,"height":1080},"scale":2}]"#;
        let output = read_output(reply).unwrap();
        assert_eq!(output.logical_size(), (960, 540));
        assert!(read_output("[]").is_err());
        let error = sway_result(r#"[{"success":false,"error":"unsupported mode"}]"#).unwrap_err();
        assert_eq!(error.to_string(), "unsupported mode");
    }

    #[test]
    fn reads_rfb_server_init_without_requesting_exclusive_access() {
        struct Exchange {
            replies: std::io::Cursor<Vec<u8>>,
            sent: Vec<u8>,
        }
        impl Read for Exchange {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.replies.read(buffer)
            }
        }
        impl Write for Exchange {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.sent.write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut stream = Exchange {
            replies: std::io::Cursor::new(
                b"RFB 003.008\n\x02\x02\x01\0\0\0\0\x07\x80\x04\x38".to_vec(),
            ),
            sent: Vec::new(),
        };
        assert_eq!(rfb_size(&mut stream).unwrap(), (1920, 1080));
        assert_eq!(stream.sent, b"RFB 003.008\n\x01\x01");
    }

    #[test]
    fn detects_wayland_in_a_rewritten_chrome_process_title() {
        assert!(chrome_uses_wayland(&[format!(
            "{CHROME} --ozone-platform=wayland"
        )]));
        assert!(!chrome_uses_wayland(&[
            CHROME.into(),
            "--ozone-platform=x11".into()
        ]));
    }
}
