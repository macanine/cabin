//! QEMU process orchestration, firmware discovery, and SSH helpers.

use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use url::Url;

use crate::Result;
use crate::config::{CHROME_URL, Config};
use crate::device::render as device_render;
use crate::paths::InstancePaths;
use crate::qmp::{QgaClient, QmpClient, qmp_query_status};
use crate::startup::{Attempt, Phase};
use crate::util::{
    RunOptions, capture, disk_free_bytes, ensure_dir, format_bytes, log_line, message,
    parse_size_bytes, pid_alive as process_alive, port_free, run, tail_file, which,
};

#[derive(Debug, Clone)]
pub struct ResolvedQemu {
    pub qemu_bin: String,
    pub firmware_code: PathBuf,
    pub firmware_vars: PathBuf,
    pub firmware_dir: Option<PathBuf>,
}

/// `reconnect-ms=` replaced `reconnect=` (seconds) for stream netdevs in
/// QEMU 9.2. Either way QEMU keeps retrying, so the VM regains its network
/// when the gateway (the Mac app) comes back.
pub(crate) fn stream_reconnect_option(qemu: &str) -> String {
    // The pinned Mac engine supports reconnect-ms; only distro builds vary.
    if cfg!(target_os = "macos") {
        return "reconnect-ms=1000".into();
    }
    let output = capture(
        &[qemu.to_string(), "--version".to_string()],
        Duration::from_secs(10),
        None,
    )
    .output;
    if qemu_at_least(&output, 9, 2) {
        "reconnect-ms=1000".to_string()
    } else {
        "reconnect=1".to_string()
    }
}

/// Parse "QEMU emulator version 9.2.0" and compare major.minor.
fn qemu_at_least(version_output: &str, major: u32, minor: u32) -> bool {
    version_output
        .split_whitespace()
        .skip_while(|word| *word != "version")
        .nth(1)
        .and_then(|version| {
            let mut parts = version.split(['.', '-', '(']);
            let a: u32 = parts.next()?.parse().ok()?;
            let b: u32 = parts.next()?.parse().ok()?;
            Some((a, b) >= (major, minor))
        })
        .unwrap_or(true)
}

/// The built-in QEMU (see `qemu_engine`) and its firmware.
pub fn resolve_qemu(cfg: &Config) -> Result<ResolvedQemu> {
    let engine = crate::qemu_engine::require(&cfg.home)?;
    let qemu_bin = engine.qemu.display().to_string();
    Ok(ResolvedQemu {
        qemu_bin,
        firmware_code: engine.firmware_code,
        firmware_vars: engine.firmware_vars,
        firmware_dir: Some(engine.firmware_dir),
    })
}

/// `qemu-img` from the built-in QEMU.
pub fn resolve_qemu_img(cfg: &Config) -> Result<PathBuf> {
    Ok(crate::qemu_engine::require(&cfg.home)?.qemu_img)
}

fn spice_args(cfg: &Config) -> Vec<String> {
    let socket = cfg.spice_socket().to_string_lossy().replace(',', ",,");
    let mut args = vec![
        "-spice".into(),
        format!(
            "unix=on,addr={socket},disable-ticketing=on,disable-copy-paste=on,disable-agent-file-xfer=on,image-compression=lz,streaming-video=off"
        ),
    ];
    if !cfg.usb_tablet {
        args.extend([
            "-device".into(),
            "virtio-keyboard-pci".into(),
            "-device".into(),
            "virtio-tablet-pci".into(),
        ]);
    }
    args
}

pub(crate) fn check_spice_dir(cfg: &Config) -> Result<()> {
    #[cfg(unix)]
    {
        let metadata = fs::symlink_metadata(cfg.spice_dir())?;
        if !metadata.is_dir()
            || metadata.permissions().mode() & 0o777 != 0o700
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return message("The SPICE socket directory must be private to the current user");
        }
    }
    #[cfg(not(unix))]
    {
        let metadata = fs::symlink_metadata(cfg.spice_dir())?;
        if !metadata.is_dir() {
            return message("The SPICE socket directory must be private to the current user");
        }
    }
    Ok(())
}

fn prepare_spice_dir(cfg: &Config) -> Result<()> {
    #[cfg(unix)]
    match fs::DirBuilder::new().mode(0o700).create(cfg.spice_dir()) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    #[cfg(not(unix))]
    match fs::create_dir(cfg.spice_dir()) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    check_spice_dir(cfg)?;
    // Stale sockets are only removed after verifying their parent is private.
    #[cfg(unix)]
    match fs::remove_file(cfg.spice_socket()) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
    #[cfg(not(unix))]
    {
        Ok(())
    }
}

pub fn build_qemu_args(
    cfg: &Config,
    daemonize: bool,
    serial_log: Option<&Path>,
    phase: Option<&str>,
    display_mode: Option<&str>,
) -> Result<Vec<String>> {
    if crate::proxy::should_manage(cfg.proxy.as_deref()) && cfg.effective_network_mode() != "user" {
        return message("The embedded proxy requires user networking");
    }
    let resolved = resolve_qemu(cfg)?;
    let serial_log = serial_log
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cfg.serial_log_path());
    if cfg.device_enabled() {
        let selected_phase = phase.map(ToOwned::to_owned).unwrap_or_else(|| {
            if cfg.device_runtime_marker().exists() {
                "runtime".to_string()
            } else {
                "install".to_string()
            }
        });
        let effective_mode = cfg.effective_network_mode();
        let mut args = device_render::full_args(
            cfg,
            &selected_phase,
            &resolved.qemu_bin,
            resolved.firmware_dir.as_deref(),
            &resolved.firmware_code,
            &serial_log,
            daemonize && effective_mode != "socket-vmnet",
            display_mode,
        )?;
        args.extend(crate::workspace::qemu_args(cfg)?);
        if effective_mode == "socket-vmnet" {
            let client = Path::new(&cfg.socket_vmnet_client);
            if !client.is_file()
                || fs::metadata(client)
                    .map(executable_bit)
                    .unwrap_or(true)
            {
                return message(format!(
                    "socket_vmnet_client not found: {}; install with 'brew install socket_vmnet'",
                    client.display()
                ));
            }
            if daemonize {
                return message("socket-vmnet must run in the foreground (the client passes fd 3)");
            }
            let mut prefixed = vec![
                cfg.socket_vmnet_client.clone(),
                cfg.socket_vmnet_socket.clone(),
            ];
            prefixed.append(&mut args);
            args = prefixed;
        }
        return Ok(args);
    }

    let effective_mode = cfg.effective_network_mode();
    let mut args: Vec<String>;
    if effective_mode == "socket-vmnet" {
        let client = Path::new(&cfg.socket_vmnet_client);
        if !client.is_file()
            || fs::metadata(client)
                .map(executable_bit)
                .unwrap_or(true)
        {
            return message(format!(
                "socket_vmnet_client not found: {}; install with 'brew install socket_vmnet'",
                client.display()
            ));
        }
        args = vec![
            cfg.socket_vmnet_client.clone(),
            cfg.socket_vmnet_socket.clone(),
            resolved.qemu_bin.clone(),
        ];
    } else {
        args = vec![resolved.qemu_bin.clone()];
    }
    if let Some(directory) = &resolved.firmware_dir {
        args.push("-L".to_string());
        args.push(directory.display().to_string());
    }
    // Machine type, accelerator (WHPX through `-accel`) and CPU model.
    if cfg!(windows) && crate::platform::accel().accel == "whpx" {
        // Probe once per engine; a rejected WHPX switches to TCG before launch.
        let _ = crate::accel::status_for_engine(Path::new(&resolved.qemu_bin));
    }
    let accelerator = crate::platform::accel().qemu_args(crate::arch::GuestArch::current());
    args.extend([
        "-name".to_string(),
        format!("vmctl-{}", cfg.name),
    ]);
    args.extend(accelerator);
    args.extend([
        "-smp".to_string(),
        cfg.cpus.to_string(),
        "-m".to_string(),
        cfg.memory_mb.to_string(),
        "-drive".to_string(),
        format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            resolved.firmware_code.display()
        ),
        "-drive".to_string(),
        format!(
            "if=pflash,format=raw,unit=1,file={}",
            cfg.efi_vars_path().display()
        ),
        "-drive".to_string(),
        format!(
            "if=none,id=disk,format=qcow2,file={},cache=writeback",
            cfg.disk_path().display()
        ),
        "-device".to_string(),
        "virtio-blk-pci,drive=disk,bootindex=0".to_string(),
        "-drive".to_string(),
        format!(
            "if=none,id=seed,format=raw,file={},readonly=on",
            cfg.seed_path().display()
        ),
        "-device".to_string(),
        "virtio-blk-pci,drive=seed,bootindex=2".to_string(),
        "-object".to_string(),
        if cfg!(target_os = "macos") {
            "rng-random,filename=/dev/urandom,id=rng0".to_string()
        } else {
            // Windows has no /dev/urandom; QEMU's built-in entropy source is
            // the portable equivalent (QEMU 5.0+).
            "rng-builtin,id=rng0".to_string()
        },
        "-device".to_string(),
        "virtio-rng-pci,rng=rng0".to_string(),
        "-device".to_string(),
        "virtio-serial-pci".to_string(),
        "-chardev".to_string(),
        crate::sockets::listen_chardev(&crate::sockets::qga_endpoint(cfg), "qga0", ""),
        "-device".to_string(),
        "virtserialport,chardev=qga0,name=org.qemu.guest_agent.0".to_string(),
    ]);
    let (width, height) = cfg.display_geometry()?.physical_size();
    if cfg.gpu_mode == "virtio" {
        args.push("-device".to_string());
        args.push(format!("virtio-gpu-pci,edid=on,xres={width},yres={height}"));
    } else if cfg.gpu_mode == "ramfb" {
        args.push("-device".to_string());
        args.push("ramfb".to_string());
    }
    if cfg.desktop_spice && cfg!(unix) {
        args.extend(spice_args(cfg));
    }
    if cfg.usb_tablet {
        args.extend([
            "-device".to_string(),
            "qemu-xhci,id=usb".to_string(),
            "-device".to_string(),
            "usb-tablet".to_string(),
            "-device".to_string(),
            "usb-kbd".to_string(),
        ]);
    }
    if cfg.audio_backend == "coreaudio" && cfg!(target_os = "macos") {
        args.extend([
            "-audiodev".to_string(),
            "coreaudio,id=audio0".to_string(),
            "-device".to_string(),
            "ich9-intel-hda,id=snd".to_string(),
            "-device".to_string(),
            "hda-output,bus=snd.0,cad=0,audiodev=audio0".to_string(),
        ]);
    }
    args.extend([
        "-display".to_string(),
        if display_mode.unwrap_or(&cfg.display_mode) == "cocoa" && cfg!(target_os = "macos") {
            "cocoa".to_string()
        } else {
            "none".to_string()
        },
        "-monitor".to_string(),
        "none".to_string(),
        "-chardev".to_string(),
        crate::sockets::listen_chardev(
            &crate::sockets::console_endpoint(cfg),
            "console0",
            &format!(",logfile={},logappend=on", serial_log.display()),
        ),
        "-serial".to_string(),
        "chardev:console0".to_string(),
        "-qmp".to_string(),
        crate::sockets::qmp_listen_arg(&crate::sockets::qmp_endpoint(cfg)),
        "-pidfile".to_string(),
        cfg.pidfile().display().to_string(),
    ]);
    // Gateway mode has one NIC: the gateway carries the internet, the Mac's
    // SSH forward (`net_forward`) and NTP. Other modes keep a user-network
    // NIC for SSH.
    if !cfg.isolated() {
        args.extend([
            "-netdev".to_string(),
            crate::proxy::user_netdev(cfg, "mgmt")?,
            "-device".to_string(),
            format!("virtio-net-pci,netdev=mgmt,mac={}", cfg.mac_mgmt()),
        ]);
    }
    args.extend(crate::workspace::qemu_args(cfg)?);
    match cfg.network_mode.as_str() {
        "gateway" => {
            args.extend([
                "-netdev".to_string(),
                crate::sockets::stream_netdev(
                    &crate::sockets::net_endpoint(cfg),
                    "data",
                    &stream_reconnect_option(&args[0]),
                ),
                "-device".to_string(),
                format!("virtio-net-pci,netdev=data,mac={}", cfg.mac_data()),
            ]);
        }
        "vmnet-shared" => {
            args.extend([
                "-netdev".to_string(),
                "vmnet-shared,id=data".to_string(),
                "-device".to_string(),
                format!("virtio-net-pci,netdev=data,mac={}", cfg.mac_data()),
            ]);
        }
        "vmnet-bridged" => {
            args.extend([
                "-netdev".to_string(),
                format!("vmnet-bridged,id=data,ifname={}", cfg.bridge_if),
                "-device".to_string(),
                format!("virtio-net-pci,netdev=data,mac={}", cfg.mac_data()),
            ]);
        }
        "socket-vmnet" => {
            args.extend([
                "-netdev".to_string(),
                "socket,id=data,fd=3".to_string(),
                "-device".to_string(),
                format!("virtio-net-pci,netdev=data,mac={}", cfg.mac_data()),
            ]);
        }
        _ => {}
    }
    if daemonize {
        if cfg.network_mode == "socket-vmnet" {
            return message("socket-vmnet must run in the foreground (the client passes fd 3)");
        }
        args.push("-daemonize".to_string());
    }
    Ok(args)
}

pub fn read_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .filter(|&pid| pid > 1 && i32::try_from(pid).is_ok())
}

pub fn pid_alive(pid: Option<u32>) -> bool {
    pid.map(process_alive).unwrap_or(false)
}

pub(crate) fn owns_qemu_process(pid: u32, pidfile: &Path, vm_name: &str) -> bool {
    if !pid_alive(Some(pid)) {
        return false;
    }
    #[cfg(unix)]
    let args = ["ps".into(), "-ww".into(), "-p".into(), pid.to_string(), "-o".into(), "command=".into()];
    #[cfg(windows)]
    let args = [
        "powershell.exe".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-Command".into(),
        format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8; (Get-CimInstance Win32_Process -Filter 'ProcessId = {pid}').CommandLine"),
    ];
    let result = capture(&args, Duration::from_secs(if cfg!(windows) { 15 } else { 2 }), None);
    if result.code != 0 {
        return false;
    }
    let Some((executable, arguments)) = result.output.trim().split_once(" -") else {
        return false;
    };
    let arguments = format!(" -{arguments} ");
    let qemu = crate::arch::GuestArch::current().qemu_system_file();
    Path::new(executable.trim_matches('"')).file_name().is_some_and(|name| name == qemu.as_str())
        && arguments.contains(&format!(" -name vmctl-{vm_name} "))
        && (arguments.contains(&format!(" -pidfile {} ", pidfile.display()))
            || arguments.contains(&format!(" -pidfile \"{}\" ", pidfile.display())))
}

/// Settings for starting QEMU and waiting for authenticated SSH.
pub struct Boot<'a> {
    pub ssh_timeout: Duration,
    pub serial_log: Option<&'a Path>,
    pub phase: Option<&'a str>,
    pub display_mode: Option<&'a str>,
}

impl Boot<'_> {
    pub fn new(ssh_timeout: Duration) -> Self {
        Self {
            ssh_timeout,
            serial_log: None,
            phase: None,
            display_mode: None,
        }
    }
}

pub struct Runtime {
    pub cfg: Config,
    foreground: Mutex<Option<Child>>,
}

impl Runtime {
    pub fn new(cfg: &Config) -> Self {
        Self {
            cfg: cfg.clone(),
            foreground: Mutex::new(None),
        }
    }

    pub fn pid(&self) -> Option<u32> {
        read_pid(&self.cfg.pidfile())
    }

    /// A fresh identity check before each signal, even when QMP is hung.
    pub(crate) fn signal_vm(&self, pid: u32, signal: i32) -> bool {
        if self.pid() != Some(pid) || !owns_qemu_process(pid, &self.cfg.pidfile(), &self.cfg.name) {
            return false;
        }
        crate::util::send_signal(pid, signal);
        true
    }

    fn foreground_child(&self) -> MutexGuard<'_, Option<Child>> {
        match self.foreground.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub fn reap(&self) {
        let mut guard = self.foreground_child();
        if let Some(child) = guard.as_mut()
            && child.try_wait().ok().flatten().is_some()
        {
            *guard = None;
        }
    }

    pub fn alive(&self) -> bool {
        {
            let mut guard = self.foreground_child();
            if let Some(child) = guard.as_mut()
                && child.try_wait().ok().flatten().is_some()
            {
                *guard = None;
                return false;
            }
        }
        pid_alive(self.pid())
    }

    pub fn qmp(&self, timeout: Duration) -> Option<QmpClient> {
        QmpClient::connect(&crate::sockets::qmp_endpoint(&self.cfg), timeout).ok()
    }

    pub fn running(&self) -> bool {
        if !self.alive() {
            return false;
        }
        match qmp_query_status(&crate::sockets::qmp_endpoint(&self.cfg), Duration::from_secs(3)) {
            Some(status) => matches!(
                status.get("status").and_then(Value::as_str),
                Some("running") | Some("paused")
            ),
            None => false,
        }
    }

    pub fn status(&self) -> Value {
        let pid = self.pid();
        let alive = pid_alive(pid);
        let qmp = if alive {
            qmp_query_status(&crate::sockets::qmp_endpoint(&self.cfg), Duration::from_secs(2))
        } else {
            None
        };
        let running = qmp
            .as_ref()
            .map(|status| {
                matches!(
                    status.get("status").and_then(Value::as_str),
                    Some("running") | Some("paused")
                )
            })
            .unwrap_or(false);
        json!({
            "pid": pid,
            "alive": alive,
            "running": running,
            "qmp": qmp,
            "ssh_port": self.cfg.ssh_port,
            "ps": qmp.as_ref().and_then(|status| status.get("status").cloned()).unwrap_or(Value::Null),
        })
    }

    pub fn cleanup_stale(&self) {
        self.reap();
        if !self.alive() {
            let _ = crate::chrome_viewer::stop(&self.cfg);
            let _ = crate::preview_forward::stop(&self.cfg);
            if check_spice_dir(&self.cfg).is_ok() {
                let _ = fs::remove_file(self.cfg.spice_socket());
            }
            for path in [
                self.cfg.pidfile(),
                self.cfg.qmp_socket(),
                self.cfg.qga_socket(),
                self.cfg.console_socket(),
            ] {
                // Windows instances keep sockets.json (the loopback ports are
                // reused by the next start); Unix removes the socket files.
                #[cfg(unix)]
                let _ = fs::remove_file(path);
                #[cfg(not(unix))]
                let _ = path;
            }
        }
    }

    fn wait_qmp(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.alive() {
                let tail = tail_file(&self.cfg.serial_log_path(), 60);
                return message(format!(
                    "QEMU exited during startup{}",
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!("\n--- serial log tail ---\n{tail}")
                    }
                ));
            }
            if qmp_query_status(&crate::sockets::qmp_endpoint(&self.cfg), Duration::from_secs(1)).is_some() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        message(format!(
            "QEMU QMP did not become ready within {:.0}s",
            timeout.as_secs_f64()
        ))
    }

    pub fn start(&self, log: &Arc<Mutex<File>>, boot: Boot<'_>) -> Result<bool> {
        if self.running() {
            log_line(
                log,
                &format!(
                    "VM {} is already running (pid {:?})",
                    self.cfg.name,
                    self.pid()
                ),
            );
            return Ok(false);
        }
        self.start_stopped(log, boot, None)?;
        Ok(true)
    }

    /// Launch QEMU (foreground or daemonized) and wait for its monitor.
    fn launch_qemu(
        &self,
        log: &Arc<Mutex<File>>,
        serial_log: &Path,
        phase: Option<&str>,
        display_mode: Option<&str>,
        effective_display: &str,
        foreground: bool,
    ) -> Result<()> {
        if foreground {
            let args = build_qemu_args(&self.cfg, false, Some(serial_log), phase, display_mode)?;
            let client_log_path = InstancePaths::new(self.cfg.instance_dir()).file(
                if self.cfg.effective_network_mode() == "socket-vmnet" {
                    "socket-vmnet-client.log"
                } else {
                    "display.log"
                },
            );
            log_line(
                log,
                &format!(
                    "launching QEMU foreground ({})",
                    if effective_display == "cocoa" {
                        "cocoa"
                    } else {
                        "socket-vmnet"
                    }
                ),
            );
            let client_log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&client_log_path)?;
            let mut command = Command::new(&args[0]);
            command.args(&args[1..]);
            command
                .stdin(Stdio::null())
                .stdout(Stdio::from(client_log.try_clone()?))
                .stderr(Stdio::from(client_log));
            crate::compat::no_window(&mut command);
            #[cfg(unix)]
            unsafe {
                command.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
            let child = command.spawn()?;
            *self.foreground_child() = Some(child);
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline && !self.cfg.pidfile().exists() {
                {
                    let mut guard = self.foreground_child();
                    if let Some(child) = guard.as_mut()
                        && child.try_wait()?.is_some()
                    {
                        return message(format!(
                            "QEMU exited during foreground startup\n{}",
                            tail_file(&client_log_path, 40)
                        ));
                    }
                }
                thread::sleep(Duration::from_millis(200));
            }
        } else {
            let args = build_qemu_args(&self.cfg, true, Some(serial_log), phase, display_mode)?;
            log_line(log, "launching QEMU (daemonized)");
            run(
                &args,
                log,
                RunOptions {
                    timeout: Duration::from_secs(90),
                    ..Default::default()
                },
            )?;
        }
        if let Err(error) = self.wait_qmp(Duration::from_secs(45)) {
            self.stop(log, Duration::from_secs(10));
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn start_stopped(
        &self,
        log: &Arc<Mutex<File>>,
        boot: Boot<'_>,
        attempt: Option<&Attempt>,
    ) -> Result<()> {
        let Boot {
            ssh_timeout,
            serial_log,
            phase,
            display_mode,
        } = boot;
        self.cleanup_stale();
        ensure_dir(&self.cfg.instance_dir())?;
        if self.cfg.desktop_spice {
            prepare_spice_dir(&self.cfg)?;
        }
        let serial_log = serial_log
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.cfg.serial_log_path());
        let effective_display = display_mode.unwrap_or(&self.cfg.display_mode);
        // Windows has no -daemonize: QEMU runs as a supervised child there.
        let foreground = cfg!(not(unix))
            || self.cfg.effective_network_mode() == "socket-vmnet"
            || effective_display == "cocoa"
            || self.cfg.audio_backend == "coreaudio";
        let mut retried = false;
        loop {
            let launched = self.launch_qemu(
                log,
                &serial_log,
                phase,
                display_mode,
                effective_display,
                foreground,
            );
            match launched {
                Ok(()) => break,
                Err(error) if !retried && crate::platform::accel().accel == "whpx" => {
                    // WHPX can pass the probe and still fail a real machine
                    // (device or memory limits): retry once under TCG.
                    let reason = crate::accel::classify_failure(&error.to_string());
                    log_line(
                        log,
                        &format!("WHPX start failed ({reason:?}); retrying under TCG: {error}"),
                    );
                    crate::accel::fall_back_to_tcg(reason);
                    retried = true;
                    self.cleanup_stale();
                }
                Err(error) => return Err(error),
            }
        }
        // A foreground client/QEMU must not become a zombie when the command
        // returns; hand the child to a detached reaper now that startup has
        // been verified through the pidfile/QMP.
        let reaped = self.foreground_child().take();
        if let Some(mut child) = reaped {
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
        let runtime = json!({
            "name": self.cfg.name,
            "pid": self.pid(),
            "started_at": crate::util::now_millis(),
            "ssh_port": self.cfg.ssh_port,
            "network_mode": self.cfg.network_mode,
            "serial_log": serial_log.display().to_string(),
            "qmp_socket": crate::sockets::qmp_endpoint(&self.cfg).describe(),
            "qga_socket": crate::sockets::qga_endpoint(&self.cfg).describe(),
        });
        if let Err(error) =
            crate::util::atomic_write_json(&self.cfg.runtime_path(), &runtime, 0o600)
        {
            self.stop(log, Duration::from_secs(20));
            return Err(error);
        }
        log_line(
            log,
            &format!(
                "QEMU running, pid {:?}, serial log {}",
                self.pid(),
                serial_log.display()
            ),
        );
        if let Some(attempt) = attempt {
            if let Some(pid) = self.pid() {
                attempt.add_fact("QEMU pid", pid.to_string());
            }
            attempt.complete(Phase::Qemu, "QEMU monitor is ready");
        }
        if let Some(attempt) = attempt {
            attempt.begin(Phase::Boot);
        }
        if let Err(error) = self.wait_for_ssh(log, ssh_timeout) {
            self.stop(log, Duration::from_secs(30));
            return Err(error);
        }
        if self.cfg.device_enabled() {
            let (rc, out) = self.ssh_capture(
                "sudo -n sh -c 'modprobe -r qemu_fw_cfg 2>/dev/null || true'",
                Duration::from_secs(15),
            );
            log_line(
                log,
                &format!("device post-boot fw_cfg cleanup rc={rc} {}", out.trim()),
            );
        }
        if let Some(attempt) = attempt {
            attempt.complete(Phase::Boot, "Authenticated SSH is ready");
        }
        Ok(())
    }

    pub fn stop(&self, log: &Arc<Mutex<File>>, timeout: Duration) -> bool {
        let _ = crate::preview_forward::stop(&self.cfg);
        if let Err(error) = crate::chrome_viewer::stop(&self.cfg) {
            log_line(log, &format!("Stopping the desktop viewer failed: {error}"));
        }
        let pid = self.pid();
        if !self.alive() {
            self.cleanup_stale();
            log_line(log, "VM is not running");
            return false;
        }
        let Some(pid) = pid.filter(|&pid| owns_qemu_process(pid, &self.cfg.pidfile(), &self.cfg.name)) else {
            log_line(log, "Could not verify the recorded PID as this QEMU instance");
            return false;
        };
        log_line(log, &format!("stopping VM (pid {pid})"));
        // 1. graceful guest shutdown via ssh
        let (rc, out) = self.ssh_capture("sudo -n shutdown -P now", Duration::from_secs(20));
        log_line(log, &format!("ssh shutdown rc={rc}"));
        if !out.is_empty() {
            log_line(log, &out);
        }
        let graceful_wait = Duration::from_secs((timeout.as_secs() / 3).max(20));
        let deadline = Instant::now() + graceful_wait;
        if rc == 0 {
            while Instant::now() < deadline && self.alive() {
                thread::sleep(Duration::from_secs(1));
            }
        }
        // Only wait for a transition after QEMU accepted the request.
        for (command, wait) in [("system_powerdown", graceful_wait), ("quit", Duration::from_secs(10))] {
            if !self.alive() {
                break;
            }
            if let Some(mut client) = self.qmp(Duration::from_secs(2))
                && client.execute("query-name", None).is_ok_and(|reply| {
                    reply["name"].as_str() == Some(&format!("vmctl-{}", self.cfg.name))
                })
                && client.execute(command, None).is_ok()
            {
                let deadline = Instant::now() + wait;
                while Instant::now() < deadline && self.alive() {
                    thread::sleep(Duration::from_millis(250));
                }
            }
        }
        // 4. signals
        if self.alive() {
            log_line(log, "guest did not shut down; sending SIGTERM");
            if !self.signal_vm(pid, crate::compat::signal::SIGTERM) {
                return false;
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && self.alive() {
                thread::sleep(Duration::from_millis(500));
            }
        }
        if self.alive() {
            log_line(log, "sending SIGKILL");
            if !self.signal_vm(pid, crate::compat::signal::SIGKILL) {
                return false;
            }
            thread::sleep(Duration::from_secs(1));
        }
        if self.alive() {
            log_line(log, "QEMU is still running");
            return false;
        }
        self.cleanup_stale();
        log_line(log, "VM stopped");
        true
    }

    // ---- ssh -----------------------------------------------------------
    pub fn ssh_target(&self) -> Result<(String, u16)> {
        if self.cfg.device_enabled() {
            device_render::ssh_endpoint(&self.cfg, None)
        } else {
            Ok(("127.0.0.1".to_string(), self.cfg.ssh_connect_port()))
        }
    }

    pub fn ssh_argv(&self, tty: bool) -> Result<Vec<String>> {
        if !self.cfg.ssh_key_path().exists() {
            return message(format!(
                "SSH key missing: {}",
                self.cfg.ssh_key_path().display()
            ));
        }
        let (host, port) = self.ssh_target()?;
        let ssh = crate::platform::ssh_tool("ssh")
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "ssh".to_string());
        let mut argv = vec![
            ssh,
            "-i".to_string(),
            self.cfg.ssh_key_path().display().to_string(),
            "-p".to_string(),
            port.to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=no".to_string(),
            "-o".to_string(),
            "UserKnownHostsFile=/dev/null".to_string(),
            "-o".to_string(),
            "IdentitiesOnly=yes".to_string(),
            "-o".to_string(),
            "LogLevel=ERROR".to_string(),
            "-o".to_string(),
            "ConnectTimeout=4".to_string(),
            format!("{}@{host}", self.cfg.current_username()),
        ];
        if tty {
            argv.insert(1, "-t".to_string());
        }
        Ok(argv)
    }

    pub fn ssh_capture(&self, command: &str, timeout: Duration) -> (i32, String) {
        if !self.cfg.ssh_key_path().exists() {
            return (
                255,
                format!("missing SSH key {}", self.cfg.ssh_key_path().display()),
            );
        }
        let argv = match self.ssh_argv(false) {
            Ok(mut argv) => {
                argv.push(command.to_string());
                argv
            }
            Err(error) => return (255, error.to_string()),
        };
        let result = capture(&argv, timeout, None);
        (result.code, result.output)
    }

    pub fn ssh_script(&self, script: &str, timeout: Duration) -> (i32, String) {
        if !self.cfg.ssh_key_path().exists() {
            return (
                255,
                format!("missing SSH key {}", self.cfg.ssh_key_path().display()),
            );
        }
        let argv = match self.ssh_argv(false) {
            Ok(mut argv) => {
                argv.push("sudo -n -E bash -s".to_string());
                argv
            }
            Err(error) => return (255, error.to_string()),
        };
        let result = capture(&argv, timeout, Some(script));
        (result.code, result.output)
    }

    pub fn wait_for_ssh(&self, log: &Arc<Mutex<File>>, timeout: Duration) -> Result<()> {
        let started = Instant::now();
        let mut last = String::new();
        let mut next_log = Duration::ZERO;
        let mut delay = Duration::from_millis(200);
        while started.elapsed() < timeout {
            if !self.alive() {
                return message("QEMU exited while waiting for SSH");
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            let (rc, out) = self.ssh_capture("true", remaining.min(Duration::from_secs(6)));
            if rc == 0 {
                log_line(
                    log,
                    &format!("SSH is ready after {:.1}s", started.elapsed().as_secs_f64()),
                );
                return Ok(());
            }
            last = out.trim().lines().last().unwrap_or("").to_string();
            let elapsed = started.elapsed();
            if elapsed >= next_log {
                log_line(
                    log,
                    &format!("waiting for SSH ({:.1}s) {last}", elapsed.as_secs_f64()),
                );
                next_log = elapsed + Duration::from_secs(5);
            }
            thread::sleep(delay.min(timeout.saturating_sub(elapsed)));
            delay = (delay * 2).min(Duration::from_secs(1));
        }
        message(format!(
            "SSH did not become ready within {:.0}s: {last}",
            timeout.as_secs_f64()
        ))
    }

    pub fn guest_ips(&self) -> std::collections::BTreeMap<String, String> {
        let (rc, out) = self.ssh_capture("ip -o -4 addr show", Duration::from_secs(15));
        let mut result = std::collections::BTreeMap::new();
        if rc != 0 {
            return result;
        }
        for line in out.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 && parts[2] == "inet" {
                result.insert(
                    parts[1].to_string(),
                    parts[3].split('/').next().unwrap_or("").to_string(),
                );
            }
        }
        result
    }

    pub fn qga_ping(&self) -> bool {
        if self.cfg.device_enabled() {
            return false;
        }
        match QgaClient::connect(&crate::sockets::qga_endpoint(&self.cfg), Duration::from_secs(3)) {
            Ok(mut client) => client.execute("guest-ping", None).is_ok(),
            Err(_) => false,
        }
    }
}

pub fn preflight(cfg: &Config) -> Vec<String> {
    let mut issues = Vec::new();
    if !matches!(std::env::consts::OS, "macos" | "linux" | "windows") {
        issues.push("Cabin runs VMs on macOS (and Linux for development)".to_string());
    }
    // The guest always matches the host architecture (arm64 on macOS), which
    // runs accelerated (HVF, KVM, WHPX) or emulated (TCG). Only a host with no
    // supported guest architecture is refused.
    if !crate::arch::GuestArch::host_supported() {
        issues.push(format!(
            "unsupported host architecture: {}",
            std::env::consts::ARCH
        ));
    }
    for &tool in crate::platform::required_tools() {
        let found = if matches!(tool, "ssh" | "ssh-keygen") {
            crate::platform::ssh_tool(tool)
        } else {
            which(tool)
        };
        if found.is_none() {
            issues.push(format!("required tool missing: {tool}"));
        }
    }
    if cfg.direct_vmnet() && euid_not_root() {
        issues.push(
            "QEMU's built-in vmnet backend requires root on macOS (ad-hoc entitlement signing is not accepted). Use --network socket-vmnet with the socket_vmnet root daemon (recommended), or --network user."
                .to_string(),
        );
    }
    let effective_mode = cfg.effective_network_mode();
    if effective_mode == "socket-vmnet" {
        let client = Path::new(&cfg.socket_vmnet_client);
        if !client.is_file() {
            issues.push(format!(
                "socket_vmnet_client is missing: {} (install: brew install socket_vmnet)",
                client.display()
            ));
        }
        if !Path::new(&cfg.socket_vmnet_socket).exists() {
            issues.push(format!(
                "socket_vmnet socket is missing: {} (start the daemon with: sudo brew services start socket_vmnet for shared mode; see 'vmctl bridge-setup' for bridged mode)",
                cfg.socket_vmnet_socket
            ));
        }
    }
    let base = if cfg.home.exists() {
        cfg.home.clone()
    } else {
        cfg.project_root.clone()
    };
    let free = disk_free_bytes(&base);
    let needed = parse_size_bytes(&cfg.disk_size).unwrap_or(0) + 3 * 1024u64.pow(3);
    if free < needed {
        issues.push(format!(
            "not enough free disk: {} available, {} recommended",
            format_bytes(Some(free)),
            format_bytes(Some(needed))
        ));
    }
    if let Err(error) = ensure_dir(&cfg.home).and_then(|_| ensure_dir(&cfg.tmp_dir())) {
        issues.push(format!(
            "cannot write VMCTL_HOME {}: {error}",
            cfg.home.display()
        ));
    }
    let runtime = Runtime::new(cfg);
    if effective_mode == "user" && !runtime.running() && !port_free(cfg.ssh_port, "127.0.0.1") {
        issues.push(format!(
            "TCP port {} on 127.0.0.1 is already in use; pass --ssh-port with a free port",
            cfg.ssh_port
        ));
    }
    if cfg.device_enabled()
        && let Ok(resolved) = cfg.device_resolved()
    {
        let network = resolved.get("network").unwrap_or(&Value::Null);
        let require_bridged = network
            .get("require_bridged")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mode = network
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("user");
        if require_bridged && mode == "user" {
            issues
                .push("device.network.require_bridged is true; use a bridged network".to_string());
        }
        if matches!(mode, "vmnet-shared" | "vmnet-bridged") && euid_not_root() {
            issues.push(
                    "device network mode uses QEMU's built-in vmnet backend, which requires root on macOS; use socket-vmnet with a root socket_vmnet daemon"
                        .to_string(),
                );
        }
        if mode == "socket-vmnet" {
            if !Path::new(&cfg.socket_vmnet_client).is_file() {
                issues.push(format!(
                    "socket_vmnet_client is missing: {}",
                    cfg.socket_vmnet_client
                ));
            }
            if !Path::new(&cfg.socket_vmnet_socket).exists() {
                issues.push(format!(
                        "socket_vmnet socket is missing: {} (start the daemon with sudo brew services start socket_vmnet)",
                        cfg.socket_vmnet_socket
                    ));
            }
        }
    }
    issues
}

pub fn preflight_warnings(cfg: &Config) -> Vec<String> {
    let mut warnings = Vec::new();
    if cfg.device_enabled()
        && let Ok(resolved) = cfg.device_resolved()
    {
        let network = resolved.get("network").unwrap_or(&Value::Null);
        let allow_user = resolved
            .get("verify")
            .and_then(|value| value.get("allow_user_network"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if network
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("user")
            == "user"
            && !allow_user
        {
            warnings.push(
                    "device is enabled but the network mode is 'user'; the guest will see slirp's 10.0.2.15/10.0.2.2/10.0.2.3 and the verifier will fail the network group. Use --network socket-vmnet with a bridged socket_vmnet daemon."
                        .to_string(),
                );
        }
    }
    if let Some(proxy) = &cfg.proxy
        && let Ok(parsed) = Url::parse(proxy)
    {
        let hostname = parsed.host_str().unwrap_or("");
        if matches!(hostname, "127.0.0.1" | "localhost" | "::1") && cfg.uses_vmnet() {
            warnings.push(format!(
                    "proxy is on localhost; guest traffic will use the rewritten address {}:{}. The proxy must listen on 0.0.0.0 (not only 127.0.0.1).",
                    cfg.host_ip_for_guest(),
                    parsed.port().map(|value| value.to_string()).unwrap_or_default()
                ));
        }
    }
    let _ = CHROME_URL;
    warnings
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn foreground_child_state_recovers_after_lock_poisoning() {
        let config = crate::Config {
            home: temp_root(),
            name: "test".into(),
            ..crate::Config::default()
        };
        let runtime = std::sync::Arc::new(super::Runtime::new(&config));
        #[cfg(unix)]
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        #[cfg(not(unix))]
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit 0"])
            .spawn()
            .unwrap();
        child.wait().unwrap();
        let other = runtime.clone();
        let result = std::thread::spawn(move || {
            let mut guard = other.foreground.lock().unwrap();
            *guard = Some(child);
            panic!("poison test lock");
        })
        .join();
        assert!(result.is_err());
        runtime.reap();
        assert!(runtime.foreground_child().is_none());
        assert!(!runtime.alive());
    }

    fn temp_root() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "vmctl-qemu-selection-{}-{nonce}",
            std::process::id()
        ))
    }
}

/// Unix checks the executable bit; Windows files are executable by extension.
fn executable_bit(metadata: std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

/// Unix-only root check for the vmnet modes (never true on Windows).
fn euid_not_root() -> bool {
    #[cfg(unix)]
    let not_root = unsafe { libc::geteuid() } != 0;
    #[cfg(not(unix))]
    let not_root = true;
    not_root
}
