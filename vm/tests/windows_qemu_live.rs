//! Native Windows check using Cabin's vendored QEMU. The app's argv opens
//! QMP and the gateway's AF_UNIX socket, and the stop flow tears QEMU down.
//!
//! Set `CABIN_VENDOR_DIR` to the directory staged by `npm run vendor`, then
//! run `cargo test --test windows_qemu_live -- --ignored --nocapture`.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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
        "cq-{name}-{}-{}",
        std::process::id(),
        vm_core::util::now_millis()
    ));
    std::fs::create_dir(&dir).expect("create an isolated QEMU test directory");
    Scratch(dir)
}

struct QemuChild(Child);

impl Drop for QemuChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn bundled_engine(home: &Path) -> vm_core::qemu_engine::Engine {
    vm_core::qemu_engine::require(home)
        .expect("set CABIN_VENDOR_DIR to the staged Windows vendor bundle")
}

fn start_qemu(args: &[String]) -> QemuChild {
    let mut command = Command::new(&args[0]);
    command.args(&args[1..]).stdin(Stdio::null());
    vm_core::util::no_window(&mut command);
    QemuChild(command.spawn().expect("launch bundled QEMU"))
}

fn wait_for_qmp(cfg: &Config, child: &mut QemuChild, args: &[String]) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if let Some(status) =
            vm_core::qmp::qmp_query_status(&cfg.qmp_socket(), Duration::from_secs(2))
        {
            assert_eq!(status["status"], "running");
            return;
        }
        if let Some(status) = child.0.try_wait().expect("read QEMU exit status") {
            panic!("QEMU exited during startup ({status}); args: {args:?}");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    panic!("QMP never became ready; args: {args:?}");
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
    std::fs::create_dir_all(cfg.serial_log_path().parent().unwrap()).unwrap();
    cfg
}

fn prepare_drives(cfg: &Config) {
    let engine = bundled_engine(&cfg.home);
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
#[ignore = "requires the staged Windows vendor bundle"]
fn bundled_engine_reports_its_version() {
    let engine = bundled_engine(Path::new("unused-home"));
    let version = vm_core::qemu_engine::version(&engine).unwrap();
    assert!(version.contains("QEMU"), "{version}");
}

#[test]
#[ignore = "requires the staged Windows vendor bundle"]
fn the_app_argv_boots_qmp_and_stops_cleanly() {
    let scratch = scratch("boot");
    let cfg = boot_config(&scratch.0.join("v,m"), "user");
    prepare_drives(&cfg);

    // The exact argv the app launches (Windows runs QEMU as a supervised
    // child instead of daemonizing).
    let args = vm_core::vm::build_qemu_args(&cfg, false, None, None, None).unwrap();
    let whpx = args
        .windows(2)
        .any(|pair| pair[0] == "-accel" && pair[1].split(',').next() == Some("whpx"));
    let tcg = args
        .windows(2)
        .any(|pair| pair[0] == "-machine" && pair[1].split(',').any(|part| part == "accel=tcg"));
    assert!(whpx || tcg, "Windows runs WHPX or TCG: {args:?}");
    assert!(args.iter().any(|arg| arg.contains("rng-builtin")));
    assert!(
        args.iter().any(|arg| arg.contains("q35")),
        "x64 guests use q35"
    );

    let mut child = start_qemu(&args);
    wait_for_qmp(&cfg, &mut child, &args);

    // The full stop flow: QMP quit, then termination, then cleanup.
    let runtime = Runtime::new(&cfg);
    let log = log(&cfg.instance_logs_dir().join("windows-live.log"));
    let stopped = runtime.stop(&log, Duration::from_secs(60));
    assert!(stopped, "stop flow shuts QEMU down");
    assert!(!runtime.alive());
    assert!(child.0.try_wait().expect("read QEMU exit status").is_some());
    assert!(!cfg.pidfile().exists());
    assert!(!cfg.qmp_socket().exists());
}

#[test]
#[ignore = "requires the staged Windows vendor bundle"]
fn gateway_mode_connects_qemu_to_the_gateway_socket() {
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

    let mut child = start_qemu(&args);
    wait_for_qmp(&cfg, &mut child, &args);
    // QEMU's data NIC dials the gateway's socket.
    let connected = Instant::now() + Duration::from_secs(30);
    while !gateway.connected() && Instant::now() < connected {
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(gateway.connected(), "QEMU connected to the gateway socket");
}
