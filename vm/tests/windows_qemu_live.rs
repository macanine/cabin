//! Opt-in live check of the Windows QEMU integration: the distro engine and
//! firmware are discovered, the exact app argv boots QEMU under TCG, the QMP
//! control channel answers on its AF_UNIX socket, and the stop flow tears
//! everything down. Skipped silently when QEMU is not on PATH.
//!
//! cargo test --test windows_qemu_live -- --ignored --nocapture
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vm_core::vm::Runtime;
use vm_core::{Config, build_config};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!(
        "cabin-qemu-live-{name}-{}-{}",
        std::process::id(),
        vm_core::util::now_millis()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

fn qemu_available() -> bool {
    vm_core::qemu_engine::installed(Path::new("unused-home")).is_some()
}

fn log(path: &Path) -> Arc<Mutex<std::fs::File>> {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    Arc::new(Mutex::new(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap(),
    ))
}

fn boot_config(home: &Path, mode: &str) -> Config {
    // Tests run in parallel: give each one its own free loopback SSH port.
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cfg = build_config(
        &serde_json::json!({
            "name": "agent",
            "home": home.display().to_string(),
            "network_mode": mode,
            "ssh_port": port,
        }),
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .unwrap();
    std::fs::create_dir_all(cfg.instance_dir()).unwrap();
    cfg
}

fn prepare_drives(cfg: &Config) {
    let engine = vm_core::qemu_engine::installed(&cfg.home)
        .expect("distro QEMU with firmware should resolve");
    assert!(engine.qemu.is_file(), "{}", engine.qemu.display());
    assert!(engine.qemu_img.is_file(), "{}", engine.qemu_img.display());
    assert!(
        engine.firmware_code.is_file(),
        "{}",
        engine.firmware_code.display()
    );
    // The disk step seeds the writable UEFI variables from the engine's copy.
    std::fs::copy(&engine.firmware_vars, cfg.efi_vars_path()).unwrap();
    for (path, format, size) in [
        (cfg.disk_path(), "qcow2", "64M"),
        (cfg.seed_path(), "raw", "1M"),
    ] {
        let created = vm_core::util::capture(
            &[
                engine.qemu_img.display().to_string(),
                "create".into(),
                "-f".into(),
                format.into(),
                path.display().to_string(),
                size.into(),
            ],
            Duration::from_secs(60),
            None,
        );
        assert_eq!(created.code, 0, "{}", created.output);
    }
}

#[test]
#[ignore = "needs QEMU for Windows on PATH"]
fn distro_engine_and_firmware_are_discovered() {
    if !qemu_available() {
        eprintln!("skipping: QEMU is not on PATH");
        return;
    }
    let engine = vm_core::qemu_engine::installed(Path::new("unused-home"))
        .expect("distro QEMU with firmware should resolve");
    let version = vm_core::qemu_engine::version(&engine).unwrap();
    assert!(version.contains("QEMU"), "{version}");
}

#[test]
#[ignore = "needs QEMU for Windows on PATH"]
fn the_app_argv_boots_qmp_and_stops_cleanly() {
    if !qemu_available() {
        eprintln!("skipping: QEMU is not on PATH");
        return;
    }
    let scratch = scratch("boot");
    let cfg = boot_config(&scratch.0.join("vm"), "user");
    prepare_drives(&cfg);

    // The exact argv the app launches (Windows runs QEMU as a supervised
    // child instead of daemonizing).
    let args = vm_core::vm::build_qemu_args(&cfg, false, None, None, None).unwrap();
    assert!(
        args.iter().any(|arg| arg.contains("accel=whpx") || arg.contains("accel=tcg")),
        "Windows runs WHPX or TCG: {:?}",
        args
    );
    assert!(args.iter().any(|arg| arg.contains("rng-builtin")));
    assert!(args.iter().any(|arg| arg.contains("q35")), "x64 guests use q35");

    let mut command = Command::new(&args[0]);
    command.args(&args[1..]);
    // Detached stdio: inherited pipes would make `cargo test` wait for QEMU.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    vm_core::compat::no_window(&mut command);
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("QEMU spawn failed: {error}"));

    // QMP answers on the instance's AF_UNIX socket as soon as QEMU is up.
    let endpoint = cfg.qmp_socket();
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut status = None;
    while Instant::now() < deadline {
        if let Some(reply) = vm_core::qmp::qmp_query_status(&endpoint, Duration::from_secs(2)) {
            status = Some(reply);
            break;
        }
        if child.try_wait().ok().flatten().is_some() {
            panic!("QEMU exited during startup; args: {args:?}");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    let status = status.expect("QMP never became ready");
    assert_eq!(status["status"], "running");

    // The full stop flow: QMP quit, then termination, then cleanup.
    let runtime = Runtime::new(&cfg);
    let log = log(&cfg.instance_logs_dir().join("windows-live.log"));
    let stopped = runtime.stop(&log, Duration::from_secs(60));
    assert!(stopped, "stop flow shuts QEMU down");
    assert!(!runtime.alive());
    let _ = child.wait();
}

#[test]
#[ignore = "needs QEMU for Windows on PATH"]
fn gateway_mode_connects_qemu_to_the_gateway_socket() {
    if !qemu_available() {
        eprintln!("skipping: QEMU is not on PATH");
        return;
    }
    let scratch = scratch("gateway");
    let cfg = boot_config(&scratch.0.join("vm"), "gateway");
    prepare_drives(&cfg);

    // The gateway listens on the instance's data socket before QEMU starts.
    let socket = cfg.net_socket();
    let gateway = vm_core::netgw::Gateway::start_closed(
        &socket,
        Vec::new(),
        None,
        (std::net::Ipv4Addr::LOCALHOST, 0).into(),
    )
    .unwrap_or_else(|error| panic!("gateway start failed: {error}"));

    let args = vm_core::vm::build_qemu_args(&cfg, false, None, None, None).unwrap();
    let netdev = args
        .iter()
        .find(|arg| arg.contains("stream,id=data"))
        .expect("gateway netdev in argv");
    assert!(
        netdev.contains(&format!("addr.path={}", socket.display())),
        "QEMU must dial the gateway's socket: {netdev}"
    );

    let mut command = Command::new(&args[0]);
    command.args(&args[1..]);
    command.stdin(Stdio::null());
    vm_core::compat::no_window(&mut command);
    let mut child = command.spawn().unwrap();

    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let status =
            vm_core::qmp::qmp_query_status(&cfg.qmp_socket(), Duration::from_secs(2));
        if let Some(status) = status {
            assert_eq!(status["status"], "running");
            break;
        }
        assert!(
            child.try_wait().ok().flatten().is_none(),
            "QEMU exited during startup; args: {args:?}"
        );
        assert!(
            Instant::now() < deadline,
            "QMP never became ready; args: {args:?}"
        );
        std::thread::sleep(Duration::from_millis(300));
    }
    // QEMU's data NIC dials the gateway's socket.
    let connected = Instant::now() + Duration::from_secs(30);
    while !gateway.connected() && Instant::now() < connected {
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(gateway.connected(), "QEMU connected to the gateway socket");
    drop(child.kill());
    let _ = child.wait();
}
