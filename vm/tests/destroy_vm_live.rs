//! Opt-in test: destroy an isolated, live QEMU process and its app-owned data.
#![cfg(target_os = "macos")]
use std::time::{Duration, Instant};
use vm_core::machine::{InstallOptions, Machine};

#[test]
#[ignore = "starts a disposable QEMU process"]
fn destroy_stops_a_real_vm_and_removes_every_owned_file() {
    let base = std::env::temp_dir().join(format!("cabin-destroy-live-{}", std::process::id()));
    let home = base.join("vm");
    let shared = base.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(shared.join("user-file"), "keep").unwrap();
    let machine = Machine::new(&home, env!("CARGO_MANIFEST_DIR"));
    let cfg = machine.install_config(&InstallOptions::default()).unwrap();
    std::fs::create_dir_all(cfg.instance_dir()).unwrap();
    std::fs::create_dir_all(home.join("images")).unwrap();
    std::fs::write(home.join("images/cached-image"), "image").unwrap();
    std::fs::write(cfg.disk_path(), "disk").unwrap();
    cfg.save_instance_meta().unwrap();
    let engine_home = std::env::var_os("CABIN_TEST_ENGINE_HOME")
        .expect("set CABIN_TEST_ENGINE_HOME to an installed VM home");
    let engine = vm_core::qemu_engine::require(std::path::Path::new(&engine_home)).unwrap();
    let output = std::process::Command::new(engine.qemu)
        .args([
            "-name",
            "vmctl-agent",
            "-machine",
            "virt,accel=hvf",
            "-cpu",
            "host",
            "-m",
            "512",
            "-S",
            "-display",
            "none",
            "-monitor",
            "none",
            "-serial",
            "none",
            "-qmp",
        ])
        .arg(format!(
            "unix:{},server=on,wait=off",
            cfg.qmp_socket().display()
        ))
        .arg("-pidfile")
        .arg(cfg.pidfile())
        .arg("-daemonize")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "QEMU: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pid = vm_core::vm::read_pid(&cfg.pidfile()).expect("QEMU pid");
    let socket = vm_core::sockets::qmp_endpoint(&cfg);
    let deadline = Instant::now() + Duration::from_secs(10);
    while vm_core::qmp::QmpClient::connect(&socket, Duration::from_secs(1)).is_err() {
        assert!(Instant::now() < deadline, "QEMU did not open QMP");
        std::thread::sleep(Duration::from_millis(100));
    }
    // Simulate the guest completing a graceful shutdown so the test does not
    // spend the full powerdown timeout waiting for a diskless VM.
    let qmp = socket.clone();
    let shutdown = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        if let Ok(mut client) = vm_core::qmp::QmpClient::connect(&qmp, Duration::from_secs(2)) {
            let _ = client.execute("quit", None);
        }
    });
    let result = machine.destroy();
    let _ = shutdown.join();
    if vm_core::vm::pid_alive(Some(pid))
        && let Ok(mut client) = vm_core::qmp::QmpClient::connect(&socket, Duration::from_secs(2))
    {
        let _ = client.execute("quit", None);
    }
    assert!(result.is_ok(), "destroy failed: {result:?}");
    assert!(!vm_core::vm::pid_alive(Some(pid)), "QEMU is still running");
    assert!(!home.join("instances").exists() && !home.join("images").exists());
    assert_eq!(
        std::fs::read_to_string(shared.join("user-file")).unwrap(),
        "keep"
    );
    println!(
        "Live QEMU stopped; machine disk, cached image and settings removed; shared file preserved"
    );
    let _ = std::fs::remove_dir_all(base);
}
