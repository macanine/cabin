//! QEMU process orchestration, firmware discovery, and SSH helpers.

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
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
    LockExt, capture, disk_free_bytes, ensure_dir, format_bytes, log_line, message,
    parse_size_bytes, pid_alive as process_alive, port_free, which,
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

// QEMU's comma-separated option syntax uses two commas for a literal comma.
fn qemu_path(path: &Path) -> String {
    path.to_string_lossy().replace(',', ",,")
}

fn spice_args(cfg: &Config) -> Vec<String> {
    let socket = qemu_path(&cfg.spice_socket());
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
    let metadata = fs::symlink_metadata(cfg.spice_dir())?;
    if !metadata.is_dir() || !crate::platform::owner_only(&metadata) {
        return message("The SPICE socket directory must be private to the current user");
    }
    Ok(())
}

fn prepare_spice_dir(cfg: &Config) -> Result<()> {
    match fs::create_dir(cfg.spice_dir()) {
        Ok(()) => crate::util::chmod(&cfg.spice_dir(), 0o700)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    check_spice_dir(cfg)?;
    crate::sockets::prepare_listener(&cfg.spice_socket())
}

pub const CLI_LANES: [&str; 3] = ["exec", "sync", "watch"];

pub fn cli_dir(cfg: &Config) -> PathBuf {
    InstancePaths::new(cfg.instance_dir()).cli_dir()
}

fn cli_sockets(cfg: &Config) -> impl Iterator<Item = PathBuf> {
    let dir = cli_dir(cfg);
    CLI_LANES
        .into_iter()
        .map(move |lane| dir.join(format!("{lane}.sock")))
}

#[cfg(unix)]
fn prepare_cli_sockets(cfg: &Config) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let dir = cli_dir(cfg);
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return message(format!(
            "Native CLI socket directory is not private: {}",
            dir.display()
        ));
    }
    for path in cli_sockets(cfg) {
        crate::sockets::prepare_listener(&path)?;
    }
    Ok(())
}

fn append_cli_args(cfg: &Config, args: &mut Vec<String>) {
    if !cfg!(unix) {
        return;
    }
    args.extend(["-device".into(), "virtio-serial-pci,id=cabincli".into()]);
    for (lane, socket) in CLI_LANES.into_iter().zip(cli_sockets(cfg)) {
        args.extend([
            "-chardev".into(),
            format!(
                "socket,id=cabin-{lane},path={},server=on,wait=off",
                qemu_path(&socket)
            ),
            "-device".into(),
            format!("virtserialport,bus=cabincli.0,chardev=cabin-{lane},name=org.cabin.{lane}"),
        ]);
    }
}

pub fn validate_socket_paths(cfg: &Config) -> Result<()> {
    if cfg!(unix) {
        for path in cli_sockets(cfg) {
            crate::sockets::validate_path(&path)?;
        }
    }
    for path in [cfg.qmp_socket(), cfg.qga_socket(), cfg.console_socket()] {
        crate::sockets::validate_path(&path)?;
    }
    if cfg.isolated() {
        crate::sockets::validate_path(&cfg.net_socket())?;
    }
    if cfg!(unix) && cfg.desktop_spice {
        crate::sockets::validate_path(&cfg.spice_socket())?;
    }
    Ok(())
}

fn prepare_qemu_sockets(cfg: &Config) -> Result<()> {
    #[cfg(unix)]
    prepare_cli_sockets(cfg)?;
    for path in [cfg.qmp_socket(), cfg.qga_socket(), cfg.console_socket()] {
        crate::sockets::prepare_listener(&path)?;
    }
    if cfg!(unix) && cfg.desktop_spice {
        prepare_spice_dir(cfg)?;
    }
    Ok(())
}

fn ssh_port_conflict(cfg: &Config) -> Option<String> {
    (!cfg.isolated() && !port_free(cfg.ssh_port, "127.0.0.1")).then(|| {
        format!("SSH port {} on 127.0.0.1 is already in use. Free that port or choose a different SSH port.", cfg.ssh_port)
    })
}

pub fn build_qemu_args(
    cfg: &Config,
    daemonize: bool,
    serial_log: Option<&Path>,
    phase: Option<&str>,
    display_mode: Option<&str>,
) -> Result<Vec<String>> {
    validate_socket_paths(cfg)?;
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
        append_cli_args(cfg, &mut args);
        if effective_mode == "socket-vmnet" {
            let client = Path::new(&cfg.socket_vmnet_client);
            if which(&cfg.socket_vmnet_client).is_none() {
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
        if which(&cfg.socket_vmnet_client).is_none() {
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
    args.extend(["-name".to_string(), format!("vmctl-{}", cfg.name)]);
    args.extend(accelerator);
    args.extend([
        "-smp".to_string(),
        cfg.cpus.to_string(),
        "-m".to_string(),
        cfg.memory_mb.to_string(),
        "-drive".to_string(),
        format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            qemu_path(&resolved.firmware_code)
        ),
        "-drive".to_string(),
        format!(
            "if=pflash,format=raw,unit=1,file={}",
            qemu_path(&cfg.efi_vars_path())
        ),
        "-drive".to_string(),
        format!(
            "if=none,id=disk,format=qcow2,file={},cache=writeback",
            qemu_path(&cfg.disk_path())
        ),
        "-device".to_string(),
        "virtio-blk-pci,drive=disk,bootindex=0".to_string(),
        "-drive".to_string(),
        format!(
            "if=none,id=seed,format=raw,file={},readonly=on",
            qemu_path(&cfg.seed_path())
        ),
        "-device".to_string(),
        "virtio-blk-pci,drive=seed,bootindex=2".to_string(),
        "-object".to_string(),
        "rng-builtin,id=rng0".to_string(),
        "-device".to_string(),
        "virtio-rng-pci,rng=rng0".to_string(),
        "-device".to_string(),
        "virtio-serial-pci".to_string(),
        "-chardev".to_string(),
        format!(
            "socket,path={},server=on,wait=off,id=qga0",
            qemu_path(&cfg.qga_socket())
        ),
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
        format!(
            "socket,id=console0,path={},server=on,wait=off,logfile={},logappend=on",
            qemu_path(&cfg.console_socket()),
            qemu_path(&serial_log)
        ),
        "-serial".to_string(),
        "chardev:console0".to_string(),
        "-qmp".to_string(),
        format!("unix:{},server=on,wait=off", qemu_path(&cfg.qmp_socket())),
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
    append_cli_args(cfg, &mut args);
    match cfg.network_mode.as_str() {
        "gateway" => {
            args.extend([
                "-netdev".to_string(),
                format!(
                    "stream,id=data,server=off,addr.type=unix,addr.path={},{}",
                    qemu_path(&cfg.net_socket()),
                    stream_reconnect_option(&args[0])
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

#[derive(PartialEq, Eq)]
enum ProcessOwner {
    Instance,
    Unrelated,
    Unknown,
}

fn process_owner(pid: u32, pidfile: &Path, vm_name: &str) -> ProcessOwner {
    let Some(argv) = crate::util::process_command(pid) else {
        return ProcessOwner::Unrelated;
    };
    let Some(executable) = argv.first().and_then(|exe| Path::new(exe).file_name()) else {
        return ProcessOwner::Unknown;
    };
    let qemu = crate::arch::GuestArch::current().qemu_system_file();
    if executable != qemu.as_str() {
        return ProcessOwner::Unrelated;
    }
    let option = |flag| {
        argv.windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].trim_matches('"'))
    };
    match (option("-name"), option("-pidfile")) {
        (Some(name), Some(file))
            if name == format!("vmctl-{vm_name}") && Path::new(file) == pidfile =>
        {
            ProcessOwner::Instance
        }
        // An inaccessible command line must not permit another launch or a signal.
        _ if argv.len() == 1 => ProcessOwner::Unknown,
        _ => ProcessOwner::Unrelated,
    }
}

pub(crate) fn recorded_qemu_pid(instance: &InstancePaths, vm_name: &str) -> Result<Option<u32>> {
    let pidfile = instance.pidfile();
    let runtime_pid = || {
        crate::util::read_json(&instance.runtime())
            .and_then(|record| record["pid"].as_u64())
            .and_then(|pid| u32::try_from(pid).ok())
    };
    for pid in read_pid(&pidfile)
        .into_iter()
        .chain(std::iter::once_with(runtime_pid).flatten())
    {
        match process_owner(pid, &pidfile, vm_name) {
            ProcessOwner::Instance => return Ok(Some(pid)),
            ProcessOwner::Unrelated => {}
            ProcessOwner::Unknown => {
                return message(format!(
                    "Cannot identify the recorded QEMU process (PID {pid}). Stop it before starting or resetting the machine."
                ));
            }
        }
    }
    Ok(None)
}

/// All runtime handles for one machine share the launch barrier. Shutdown sets
/// its flag before waiting on the barrier, so a provisioning step cannot boot
/// a new QEMU after shutdown has stopped the previous one.
#[derive(Default)]
pub(crate) struct Lifecycle {
    gate: Mutex<()>,
    quitting: AtomicBool,
    cancelled: AtomicBool,
}

impl Lifecycle {
    pub(crate) fn for_home(home: &Path) -> Arc<Self> {
        static HOMES: OnceLock<Mutex<HashMap<PathBuf, Weak<Lifecycle>>>> = OnceLock::new();
        let mut homes = HOMES.get_or_init(Default::default).locked();
        homes.retain(|_, handle| handle.strong_count() != 0);
        if let Some(handle) = homes.get(home).and_then(Weak::upgrade) {
            return handle;
        }
        let handle = Arc::new(Self::default());
        homes.insert(home.to_path_buf(), Arc::downgrade(&handle));
        handle
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.quitting.load(Ordering::Acquire) {
            return message("Cabin is quitting");
        }
        if self.cancelled.load(Ordering::Acquire) {
            return message("Install cancelled");
        }
        Ok(())
    }

    pub(crate) fn begin_quit(&self) {
        self.quitting.store(true, Ordering::Release);
    }

    pub(crate) fn set_cancelled(&self, cancelled: bool) {
        self.cancelled.store(cancelled, Ordering::Release);
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, ()> {
        self.gate.locked()
    }
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
    lifecycle: Arc<Lifecycle>,
}

impl Runtime {
    pub fn new(cfg: &Config) -> Self {
        Self {
            cfg: cfg.clone(),
            foreground: Mutex::new(None),
            lifecycle: Lifecycle::for_home(&cfg.home),
        }
    }

    fn recorded_pid(&self) -> Result<Option<u32>> {
        recorded_qemu_pid(&InstancePaths::new(self.cfg.instance_dir()), &self.cfg.name)
    }

    pub fn pid(&self) -> Option<u32> {
        self.recorded_pid().ok().flatten()
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
        self.reap();
        // An uninspectable PID is conservatively treated as live.
        !matches!(self.recorded_pid(), Ok(None))
    }

    pub fn qmp(&self, timeout: Duration) -> Option<QmpClient> {
        QmpClient::connect(&self.cfg.qmp_socket(), timeout).ok()
    }

    pub fn running(&self) -> bool {
        if !self.alive() {
            return false;
        }
        match qmp_query_status(&self.cfg.qmp_socket(), Duration::from_secs(3)) {
            Some(status) => matches!(
                status.get("status").and_then(Value::as_str),
                Some("running") | Some("paused")
            ),
            None => false,
        }
    }

    pub fn status(&self) -> Value {
        let pid = self.pid();
        let alive = pid.is_some();
        let qmp = if alive {
            qmp_query_status(&self.cfg.qmp_socket(), Duration::from_secs(2))
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
        if matches!(self.recorded_pid(), Ok(None)) {
            let _ = crate::chrome_viewer::stop(&self.cfg);
            let _ = crate::preview_forward::stop(&self.cfg);
            for path in [self.cfg.pidfile(), self.cfg.runtime_path()] {
                let _ = fs::remove_file(path);
            }
            for path in [
                self.cfg.qmp_socket(),
                self.cfg.qga_socket(),
                self.cfg.console_socket(),
            ] {
                let _ = crate::sockets::prepare_listener(&path);
            }
            if check_spice_dir(&self.cfg).is_ok() {
                let _ = crate::sockets::prepare_listener(&self.cfg.spice_socket());
            }
        }
    }

    fn wait_qmp(&self, child: &mut Child, stderr: &Path, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                let diagnostic = classify_qemu_exit(&crate::startup::log_tail(stderr).join("\n"));
                return message(format!(
                    "QEMU exited during startup ({status}): {diagnostic}"
                ));
            }
            self.lifecycle.check()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return message(format!(
                    "QEMU QMP did not become ready within {:.0}s. See {}",
                    timeout.as_secs_f64(),
                    stderr.display()
                ));
            }
            if self.pid().is_some()
                && qmp_query_status(
                    &self.cfg.qmp_socket(),
                    remaining.min(Duration::from_secs(1)),
                )
                .is_some()
            {
                return Ok(());
            }
            thread::sleep(remaining.min(Duration::from_millis(100)));
        }
    }

    pub(crate) fn previous_vm_error<T>(&self) -> Result<T> {
        self.recorded_pid()?;
        if self.cfg.instance_file("sockets.json").exists() {
            return message(
                "A VM from an earlier version of Cabin is still running with TCP control sockets. Stop it in the earlier version of Cabin or restart this computer, then start it here.",
            );
        }
        message("The QEMU process exists, but its monitor did not report a running machine")
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

    /// Run QEMU as a child on every host so stderr and its exit status are
    /// available even if it dies before opening QMP or writing its PID.
    fn launch_qemu(
        &self,
        log: &Arc<Mutex<File>>,
        args: &[String],
        serial_log: &Path,
    ) -> Result<()> {
        // QEMU opens the serial capture itself. Prepare its directory and
        // permissions before passing the path to the chardev.
        cabin_diagnostics::open_capture(serial_log, cabin_diagnostics::Capture::Replace)?;
        let stderr_path = self.cfg.instance_logs_dir().join("qemu-stderr.log");
        let stderr =
            cabin_diagnostics::open_capture(&stderr_path, cabin_diagnostics::Capture::Replace)?;
        log_line(
            log,
            &format!("launching QEMU (stderr: {})", stderr_path.display()),
        );
        let mut command = Command::new(&args[0]);
        command
            .args(&args[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr));
        crate::util::no_window(&mut command);
        crate::platform::detach_session(&mut command);
        self.lifecycle.check()?;
        let mut child = command.spawn().map_err(|error| {
            crate::Error::Message(format!("Could not launch QEMU using {}: {error}", args[0]))
        })?;
        let pid = child.id();
        let result = self
            .save_runtime(pid, serial_log)
            .and_then(|()| self.wait_qmp(&mut child, &stderr_path, Duration::from_secs(45)))
            .and_then(|()| {
                let actual_pid = self.recorded_pid()?.ok_or_else(|| {
                    crate::Error::Message("QEMU exited after opening its monitor".into())
                })?;
                if actual_pid != pid {
                    self.save_runtime(actual_pid, serial_log)?;
                }
                Ok(())
            });
        if result.is_err() && self.pid().is_none() {
            // QEMU may have stalled before writing its PID. This handle still
            // identifies the child we spawned, even without a pidfile.
            let _ = child.kill();
            let _ = child.wait();
            self.cleanup_stale();
        } else {
            *self.foreground_child() = Some(child);
        }
        result
    }

    fn save_runtime(&self, pid: u32, serial_log: &Path) -> Result<()> {
        let runtime = json!({
            "name": self.cfg.name,
            "pid": pid,
            "started_at": crate::util::now_millis(),
            "ssh_port": self.cfg.ssh_connect_port(),
            "network_mode": self.cfg.network_mode,
            "serial_log": serial_log.display().to_string(),
            "qmp_socket": self.cfg.qmp_socket().display().to_string(),
            "qga_socket": self.cfg.qga_socket().display().to_string(),
        });
        crate::util::atomic_write_json(&self.cfg.runtime_path(), &runtime, 0o600)
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
        let serial_log = serial_log
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.cfg.serial_log_path());
        self.lifecycle.check()?;
        let engine = resolve_qemu(&self.cfg)?.qemu_bin;
        let engine = Path::new(&engine);
        let stderr_path = self.cfg.instance_logs_dir().join("qemu-stderr.log");
        let boot = || {
            let setup_error = |error| (error, None);
            let launch = || -> Result<Vec<String>> {
                self.lifecycle.check()?;
                if self.recorded_pid()?.is_some() {
                    return self.previous_vm_error();
                }
                validate_socket_paths(&self.cfg)?;
                if let Some(error) = ssh_port_conflict(&self.cfg) {
                    return message(error);
                }
                ensure_dir(&self.cfg.instance_dir())?;
                self.cleanup_stale();
                migrate_legacy_sockets(&self.cfg.instance_dir())?;
                crate::accel::restore_start_fallback(&self.cfg.home, engine);
                prepare_qemu_sockets(&self.cfg)?;
                build_qemu_args(&self.cfg, false, Some(&serial_log), phase, display_mode)
            };
            let gate = self.lifecycle.lock();
            let args = launch().map_err(setup_error)?;
            // Reserve a reaper before spawning QEMU so a thread creation
            // failure cannot leave an already-running child unowned.
            let (reap, children) = std::sync::mpsc::sync_channel::<Child>(1);
            thread::Builder::new()
                .name("qemu-reaper".into())
                .spawn(move || {
                    if let Ok(mut child) = children.recv() {
                        let _ = child.wait();
                    }
                })
                .map_err(|error| {
                    setup_error(crate::Error::Message(format!(
                        "Could not start the QEMU process reaper: {error}"
                    )))
                })?;
            let whpx = args
                .windows(2)
                .any(|pair| pair[0] == "-accel" && pair[1].starts_with("whpx,"));
            let exited_whpx = || {
                whpx && self.lifecycle.check().is_ok() && matches!(self.recorded_pid(), Ok(None))
            };
            if let Err(error) = self.launch_qemu(log, &args, &serial_log) {
                let reason = exited_whpx()
                    .then(|| whpx_start_failure(&error.to_string()))
                    .flatten();
                let started = self.foreground_child().is_some();
                self.handoff_to_reaper(reap).map_err(setup_error)?;
                drop(gate);
                if started {
                    self.stop(log, Duration::from_secs(10));
                }
                return Err((error, reason));
            }
            self.handoff_to_reaper(reap).map_err(setup_error)?;
            drop(gate);
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
                attempt.accelerator_fact();
                attempt.complete(Phase::Qemu, "QEMU monitor is ready");
                attempt.begin(Phase::Boot);
            }
            if let Err(error) = self.wait_for_ssh(log, ssh_timeout) {
                // A vCPU can fail after the monitor first answers. Only a
                // confirmed WHPX exit in this launch's stderr warrants a retry.
                let reason = exited_whpx()
                    .then(|| crate::startup::log_tail(&stderr_path))
                    .and_then(|lines| lines.into_iter().rfind(|line| !line.trim().is_empty()))
                    .and_then(|line| crate::accel::classify_whpx_output(&line));
                self.stop(log, Duration::from_secs(30));
                return Err((error, reason));
            }
            Ok(())
        };
        crate::accel::retry_start_with_tcg(boot, |error, reason| {
            log_line(
                log,
                &format!("WHPX start failed ({reason:?}); retrying under TCG: {error}"),
            );
            crate::accel::remember_start_fallback(&self.cfg.home, engine, reason, log);
        })?;
        self.lifecycle.check()?;
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

    fn handoff_to_reaper(&self, reaper: std::sync::mpsc::SyncSender<Child>) -> Result<()> {
        let child = self.foreground_child().take();
        if let Some(child) = child
            && let Err(std::sync::mpsc::SendError(mut child)) = reaper.send(child)
        {
            let _ = child.kill();
            let _ = child.wait();
            self.cleanup_stale();
            return message("The QEMU process reaper exited unexpectedly");
        }
        Ok(())
    }

    pub fn stop(&self, log: &Arc<Mutex<File>>, timeout: Duration) -> bool {
        let _gate = self.lifecycle.lock();
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
        let Some(pid) = pid else {
            log_line(
                log,
                "Could not verify the recorded PID as this QEMU instance",
            );
            return false;
        };
        log_line(log, &format!("stopping VM (pid {pid})"));
        let instance = InstancePaths::new(self.cfg.instance_dir());
        let stopped = stop_process(
            pid,
            &instance,
            &self.cfg.name,
            timeout,
            |wait| {
                let (rc, out) = self.ssh_capture("sudo -n shutdown -P now", wait);
                log_line(log, &format!("ssh shutdown rc={rc} {out}"));
                rc == 0
            },
            || self.reap(),
        );
        if !stopped {
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
        // The guest's host key changes with every install, so nothing is
        // recorded. Use the null device, not `none`: OpenSSH 10.5 on macOS
        // fails `Host key verification failed` with `UserKnownHostsFile=none`
        // even under StrictHostKeyChecking=no.
        const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };
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
            format!("UserKnownHostsFile={NULL_DEVICE}"),
            "-o".to_string(),
            format!("GlobalKnownHostsFile={NULL_DEVICE}"),
            "-o".to_string(),
            "ServerAliveInterval=10".to_string(),
            "-o".to_string(),
            "ServerAliveCountMax=3".to_string(),
            "-o".to_string(),
            "IdentitiesOnly=yes".to_string(),
            "-o".to_string(),
            "LogLevel=ERROR".to_string(),
            "-o".to_string(),
            format!("ConnectTimeout={}", 4 * crate::platform::slowdown()),
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
        let (timeout, probe_timeout) = ssh_wait_budgets(timeout, crate::platform::slowdown());
        let started = Instant::now();
        let mut last = String::new();
        let mut next_log = Duration::ZERO;
        let mut delay = Duration::from_millis(200);
        while started.elapsed() < timeout {
            self.lifecycle.check()?;
            if !self.alive() {
                return message("QEMU exited while waiting for SSH");
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            let (rc, out) = self.ssh_capture("true", remaining.min(probe_timeout));
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
        match QgaClient::connect(&self.cfg.qga_socket(), Duration::from_secs(3)) {
            Ok(mut client) => client.execute("guest-ping", None).is_ok(),
            Err(_) => false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ShutdownAction {
    Guest,
    Powerdown,
    Quit,
    Terminate,
    Kill,
}

fn shutdown_in_order(
    budget: Duration,
    mut request: impl FnMut(ShutdownAction, Duration) -> bool,
    mut exited: impl FnMut(Duration) -> bool,
) -> bool {
    use ShutdownAction::*;
    // Allow time for forced exit even if SSH and the monitor consume their
    // full budgets. Native session-end notifications supply a shorter budget.
    let graceful = budget / 5;
    let control = (graceful / 2).min(Duration::from_secs(2));
    let forced = (graceful / 3).min(Duration::from_secs(2));
    for (action, timeout, wait) in [
        (Guest, graceful.min(Duration::from_secs(10)), graceful),
        (Powerdown, control, graceful),
        (Quit, control, forced),
        (Terminate, Duration::ZERO, forced),
        (Kill, Duration::ZERO, forced),
    ] {
        if exited(Duration::ZERO) || (request(action, timeout) && exited(wait)) {
            return true;
        }
    }
    exited(Duration::ZERO)
}

pub(crate) fn stop_process(
    pid: u32,
    instance: &InstancePaths,
    name: &str,
    timeout: Duration,
    mut guest_shutdown: impl FnMut(Duration) -> bool,
    mut reap: impl FnMut(),
) -> bool {
    let pidfile = instance.pidfile();
    let owner = || process_owner(pid, &pidfile, name);
    shutdown_in_order(
        timeout,
        |action, timeout| {
            if owner() != ProcessOwner::Instance {
                return false;
            }
            match action {
                ShutdownAction::Guest => guest_shutdown(timeout),
                ShutdownAction::Powerdown => {
                    qmp_shutdown(&instance.qmp_socket(), name, "system_powerdown", timeout)
                }
                ShutdownAction::Quit => qmp_shutdown(&instance.qmp_socket(), name, "quit", timeout),
                ShutdownAction::Terminate | ShutdownAction::Kill => {
                    crate::util::terminate_process(pid, action == ShutdownAction::Terminate);
                    true
                }
            }
        },
        |wait| {
            let deadline = Instant::now() + wait;
            loop {
                reap();
                if owner() == ProcessOwner::Unrelated {
                    return true;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return false;
                }
                thread::sleep(remaining.min(Duration::from_millis(100)));
            }
        },
    )
}

fn qmp_shutdown(path: &Path, name: &str, command: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    QmpClient::connect(path, timeout).is_ok_and(|mut client| {
        client
            .execute_until("query-name", None, deadline)
            .is_ok_and(|reply| reply["name"].as_str() == Some(&format!("vmctl-{name}")))
            && client.execute_until(command, None, deadline).is_ok()
    })
}

fn classify_qemu_exit(stderr: &str) -> String {
    let output = stderr.to_ascii_lowercase();
    let explanation = if output.contains("host forwarding rule")
        || (output.contains("address already in use") && output.contains("hostfwd"))
    {
        "QEMU could not bind the SSH forwarding port. Free that port and retry."
    } else if output.contains("failed to get \"write\" lock")
        || output.contains("another process using the image")
    {
        "Another process has the VM disk open."
    } else if output.contains("could not allocate memory")
        || output.contains("cannot allocate memory")
    {
        "QEMU could not allocate memory for the VM. Close other apps or reduce the VM's memory."
    } else if output.contains("no space left") {
        "The VM's storage is full."
    } else if output.contains("permission denied") || output.contains("access is denied") {
        "QEMU was denied access to a VM file or socket."
    } else if output.contains("failed to initialize hvf") || output.contains("hvf error") {
        "QEMU could not use hardware virtualization (HVF)."
    } else if stderr.is_empty() {
        "QEMU produced no stderr."
    } else {
        return stderr.to_owned();
    };
    if stderr.is_empty() {
        explanation.into()
    } else {
        format!("{explanation}\n{stderr}")
    }
}

fn whpx_start_failure(error: &str) -> Option<crate::accel::AccelProblem> {
    // Only an early QEMU exit is eligible. WHPX can print an informational
    // banner before an unrelated fatal error from a disk or network device.
    if !error.starts_with("QEMU exited during startup") {
        return None;
    }
    let last = error.lines().rfind(|line| !line.trim().is_empty())?;
    crate::accel::classify_whpx_output(last)
}

fn migrate_legacy_sockets(instance_dir: &Path) -> Result<()> {
    // Old Windows builds stored loopback control ports here. The current
    // endpoints are derived from the instance path; no disk changes are needed.
    match fs::remove_file(instance_dir.join("sockets.json")) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

fn ssh_wait_budgets(timeout: Duration, slowdown: u32) -> (Duration, Duration) {
    (
        timeout.saturating_mul(slowdown),
        Duration::from_secs(6).saturating_mul(slowdown),
    )
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
    if cfg.direct_vmnet() && !crate::platform::is_root() {
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
    // An installed qcow2 is sparse and already has its current clusters on
    // disk. Installation and requested growth have their own capacity checks.
    let install_capacity = if cfg.disk_path().is_file() {
        0
    } else {
        parse_size_bytes(&cfg.disk_size).unwrap_or(0)
    };
    let needed =
        install_capacity.saturating_add(crate::readiness::IMAGE_HEADROOM_GB * 1024u64.pow(3));
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
    if !runtime.alive()
        && let Some(error) = ssh_port_conflict(cfg)
    {
        issues.push(error);
    }
    if let Err(error) = validate_socket_paths(cfg) {
        issues.push(error.to_string());
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
        if matches!(mode, "vmnet-shared" | "vmnet-bridged") && !crate::platform::is_root() {
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
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn occupied_port_has_a_clear_error_for_qemu_forwarding() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let cfg = Config {
            network_mode: "user".into(),
            ssh_port: listener.local_addr().unwrap().port(),
            ..Config::default()
        };
        let problem = ssh_port_conflict(&cfg).unwrap();
        assert!(problem.contains(&cfg.ssh_port.to_string()));
        assert!(problem.contains("already in use"));
        let cfg = Config {
            network_mode: "gateway".into(),
            ..cfg
        };
        assert!(ssh_port_conflict(&cfg).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn early_exit_returns_status_and_classified_stderr_before_pidfile() {
        let home = crate::sockets::test_dir();
        let cfg = Config {
            home: home.clone(),
            ..Config::default()
        };
        ensure_dir(&cfg.instance_dir()).unwrap();
        let log = Arc::new(Mutex::new(File::create(home.join("start.log")).unwrap()));
        let runtime = Runtime::new(&cfg);
        let args = vec![
            "sh".into(),
            "-c".into(),
            "echo 'qemu: Could not set up host forwarding rule' >&2; exit 42".into(),
        ];
        let error = runtime
            .launch_qemu(&log, &args, &cfg.serial_log_path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("42"), "{error}");
        assert!(
            error.contains("could not bind the SSH forwarding port"),
            "{error}"
        );
        assert!(
            error.contains("qemu: Could not set up host forwarding rule"),
            "{error}"
        );
        assert!(!cfg.pidfile().exists());
        assert!(!runtime.alive());
        drop(log);
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn quit_interrupts_a_child_stuck_before_qmp_and_reaps_it() {
        let home = crate::sockets::test_dir();
        let cfg = Config {
            home: home.clone(),
            ..Config::default()
        };
        ensure_dir(&cfg.instance_dir()).unwrap();
        let runtime = Runtime::new(&cfg);
        let log = Arc::new(Mutex::new(File::create(home.join("start.log")).unwrap()));
        std::thread::scope(|scope| {
            let child = scope.spawn(|| {
                let _launch = runtime.lifecycle.lock();
                let args = vec!["sh".into(), "-c".into(), "exec sleep 30".into()];
                runtime.launch_qemu(&log, &args, &cfg.serial_log_path())
            });
            let deadline = Instant::now() + Duration::from_secs(4);
            let pid = loop {
                if let Some(pid) = crate::util::read_json(&cfg.runtime_path())
                    .and_then(|state| state["pid"].as_u64())
                {
                    break pid as u32;
                }
                assert!(Instant::now() < deadline, "child never entered startup");
                std::thread::sleep(Duration::from_millis(10));
            };
            runtime.lifecycle.begin_quit();
            let _gate = runtime.lifecycle.lock();
            let error = child.join().unwrap().unwrap_err();
            assert_eq!(error.to_string(), "Cabin is quitting");
            assert!(!pid_alive(Some(pid)));
        });
        drop(log);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn whpx_diagnostic_excludes_unrelated_start_failures() {
        let error = "QEMU exited during startup: WHPX enabled
qemu: Failed to get \"write\" lock";
        assert_eq!(whpx_start_failure(error), None);
        let error = "QEMU exited during startup: WHPX: Failed to create partition, hr=80070005";
        assert_eq!(
            whpx_start_failure(error),
            Some(crate::accel::AccelProblem::HypervisorConflict)
        );
        assert!(matches!(
            whpx_start_failure("QEMU exited during startup: whpx_init_vcpu failed: 80070057"),
            Some(crate::accel::AccelProblem::ProbeFailed(_))
        ));
    }

    #[test]
    fn shutdown_tries_graceful_paths_before_forcing_a_hung_vm() {
        use ShutdownAction::*;
        use std::cell::Cell;
        let mut actions = Vec::new();
        let gone = Cell::new(false);
        let stopped = shutdown_in_order(
            Duration::from_secs(4),
            |action, _| {
                if action == Kill {
                    gone.set(true);
                }
                actions.push(action);
                true
            },
            |_| gone.get(),
        );
        assert!(stopped);
        assert_eq!(actions, [Guest, Powerdown, Quit, Terminate, Kill]);

        gone.set(false);
        actions.clear();
        assert!(shutdown_in_order(
            Duration::from_secs(4),
            |action, _| {
                actions.push(action);
                gone.set(true);
                true
            },
            |_| gone.get(),
        ));
        assert_eq!(actions, [Guest]);
    }

    #[test]
    fn legacy_tcp_socket_file_is_discarded_without_touching_the_disk() {
        let dir = crate::sockets::test_dir();
        let instance = InstancePaths::new(&dir);
        let old = dir.join("sockets.json");
        fs::write(
            &old,
            br#"{"qmp":5555,"qga":5556,"console":5557,"net":5558}"#,
        )
        .unwrap();
        fs::write(instance.disk(), b"persistent guest").unwrap();
        migrate_legacy_sockets(&dir).unwrap();
        migrate_legacy_sockets(&dir).unwrap();
        assert!(!old.exists());
        assert_eq!(fs::read(instance.disk()).unwrap(), b"persistent guest");
        assert!(instance.qmp_socket().ends_with("qmp.sock"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_or_torn_image_verification_record_requires_a_checksum() {
        use crate::steps::base::{Step, StepContext};
        use crate::steps::global::ImageStep;
        let home = crate::sockets::test_dir();
        let cfg = Config {
            home: home.clone(),
            image_url: "https://example.invalid/image.img".into(),
            image_sha256: crate::util::sha256_bytes(b"correct image"),
            ..Config::default()
        };
        ensure_dir(&cfg.images_dir()).unwrap();
        let path = cfg.image_path();
        let cache = path.with_file_name(".image.img.verified.json");
        fs::write(&path, b"correct image").unwrap();
        let mut ctx = StepContext::new(&cfg, "image", "start").unwrap();
        assert!(ImageStep.check(&mut ctx).unwrap().ok);
        assert!(
            ImageStep
                .check(&mut ctx)
                .unwrap()
                .detail
                .contains("previously verified")
        );
        fs::write(&path, b"invalid").unwrap();
        assert!(
            ImageStep
                .check(&mut ctx)
                .unwrap()
                .detail
                .contains("checksum mismatch")
        );
        fs::write(&cache, b"{\"broken").unwrap();
        assert!(!ImageStep.check(&mut ctx).unwrap().ok);
        drop(ctx);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn foreground_child_state_recovers_after_lock_poisoning() {
        let config = crate::Config {
            home: temp_root(),
            name: "test".into(),
            ..crate::Config::default()
        };
        let runtime = std::sync::Arc::new(super::Runtime::new(&config));
        // The test binary lists its tests and exits.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
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
