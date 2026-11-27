//! Production SSH transport and sync cycle against a disposable Ubuntu guest.

use std::fs::{self, File};
use std::net::TcpListener;

use serde_json::json;
use vm_core::vm::{Boot, Runtime};

use super::*;

struct Guest {
    runtime: Runtime,
    log: Arc<Mutex<File>>,
}

impl Drop for Guest {
    fn drop(&mut self) {
        self.runtime.stop(&self.log, Duration::from_secs(45));
    }
}

fn run(command: &mut Command) {
    let answer = command.output().unwrap();
    assert!(
        answer.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&answer.stderr)
    );
}

fn guest_command(runtime: &Runtime, script: &str) -> String {
    let (code, output) = runtime.ssh_capture(script, Duration::from_secs(90));
    assert_eq!(code, 0, "{script}: {output}");
    output
}

fn check_daemon(runtime: &Runtime, build: &str) {
    let argv = vm_core::agentd::connect_argv(runtime).unwrap();
    let input = "{\"id\":1,\"method\":\"clock\",\"params\":{}}\n";
    let output = vm_core::util::capture(&argv, Duration::from_secs(30), Some(input));
    assert_eq!(output.code, 0, "{}", output.output);
    let messages: Vec<serde_json::Value> = output
        .output
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(
        messages.iter().any(|line| matches!(
            agent_core::hello::check_hello(line, build),
            agent_core::hello::Handshake::Ready(_)
        )),
        "{}",
        output.output
    );
    assert!(
        messages
            .iter()
            .any(|line| line["id"] == 1 && line["result"]["nowMs"].is_number()),
        "{}",
        output.output
    );
}

fn boot(runtime: &Runtime, log: &Arc<Mutex<File>>, expected: &str) {
    if let Err(error) = runtime.start(log, Boot::new(Duration::from_secs(300))) {
        panic!(
            "{error}\n{}",
            vm_core::util::tail_file(&runtime.cfg.instance_logs_dir().join("qemu-stderr.log"), 30)
        );
    }
    assert_eq!(vm_core::accel::status().accel, expected);
}

#[cfg(windows)]
fn prepare_accelerator(expected: &str, home: &Path, qemu: &Path) {
    assert!(matches!(expected, "whpx" | "tcg"));
    if expected == "whpx" {
        vm_core::accel::probe_whpx(qemu).expect("the WHPX runner must answer the real QMP probe");
        assert_eq!(
            vm_core::accel::status().accel,
            "whpx",
            "the runner must support WHPX"
        );
    } else {
        // Exercise the product's saved fallback path with the bundled QEMU.
        let meta = qemu.metadata().unwrap();
        let modified = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let state = vm_core::paths::StatePaths::new(home).global_state();
        // SAFETY: GetTickCount64 takes no arguments and returns monotonic uptime.
        let uptime = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
        let boot_time = u64::try_from(vm_core::util::now_millis())
            .unwrap()
            .saturating_sub(uptime)
            / 1000;
        vm_core::util::atomic_write_json(&state.join("whpx-start.json"), &json!({
            "key": {"engine": qemu, "size": meta.len(), "modified": [modified.as_secs(), u64::from(modified.subsec_nanos())]},
            "boot": boot_time,
            "reason": vm_core::accel::AccelProblem::HypervisorConflict,
        }), 0o600).unwrap();
    }
}

#[test]
#[ignore = "needs a bundled engine and an Ubuntu cloud image for the host architecture"]
fn ubuntu_ssh_sync_and_restart() {
    let image = PathBuf::from(
        std::env::var_os("CABIN_TEST_IMAGE")
            .expect("set CABIN_TEST_IMAGE to a matching Ubuntu qcow2"),
    );
    let agentd = PathBuf::from(
        std::env::var_os("CABIN_TEST_AGENTD")
            .expect("set CABIN_TEST_AGENTD to the matching Linux cabind"),
    );
    let expected = std::env::var("CABIN_TEST_ACCEL")
        .expect("set CABIN_TEST_ACCEL to the expected accelerator");
    assert!(
        vm_core::vendor::vendor().is_some(),
        "set CABIN_VENDOR_DIR to the complete host bundle"
    );
    assert!(image.is_file() && agentd.is_file());
    let scratch = tempfile::Builder::new().prefix("cw-").tempdir().unwrap();
    let home = scratch.path().join("vm");
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cfg = vm_core::build_config(
        &json!({
            "home": home, "name": "agent", "username": "dev", "hostname": "cabin-sync-test",
            "network_mode": "user", "ssh_port": port, "memory_mb": 1536, "cpus": 2,
            "disk_size": "10G"
        }),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../vm")
            .as_path(),
    )
    .unwrap();
    fs::create_dir_all(cfg.instance_dir()).unwrap();
    fs::create_dir_all(cfg.instance_logs_dir()).unwrap();
    let engine = vm_core::qemu_engine::require(&home).unwrap();
    #[cfg(windows)]
    prepare_accelerator(&expected, &home, &engine.qemu);
    #[cfg(target_os = "macos")]
    assert_eq!(expected, "hvf");

    let keygen = vm_core::platform::ssh_tool("ssh-keygen").unwrap();
    run(Command::new(keygen)
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(cfg.ssh_key_path()));
    let key = fs::read_to_string(cfg.ssh_pub_path()).unwrap();
    let seed = scratch.path().join("seed");
    fs::create_dir(&seed).unwrap();
    fs::write(seed.join("user-data"), format!(
        "#cloud-config\nusers:\n  - name: dev\n    sudo: ALL=(ALL) NOPASSWD:ALL\n    shell: /bin/bash\n    ssh_authorized_keys:\n      - {}\nssh_pwauth: false\npackage_update: false\n", key.trim()
    )).unwrap();
    fs::write(
        seed.join("meta-data"),
        "instance-id: cabin-windows-sync-test\n",
    )
    .unwrap();
    fs::write(
        seed.join("network-config"),
        vm_core::steps::seed::build_network_config(&cfg)
            .unwrap()
            .to_string(),
    )
    .unwrap();
    vm_core::platform::build_seed_iso(
        &seed,
        &cfg.seed_path(),
        &["user-data", "meta-data", "network-config"],
    )
    .unwrap();
    // The base image is read-only here. All writes land in the scratch overlay.
    run(Command::new(&engine.qemu_img)
        .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
        .arg(
            image
                .canonicalize()
                .map(|path| agent_core::sync::strip_verbatim_prefix(&path))
                .unwrap(),
        )
        .arg(cfg.disk_path())
        .arg("10G"));
    fs::copy(engine.firmware_vars, cfg.efi_vars_path()).unwrap();
    cfg.save_instance_meta().unwrap();

    let guest = Guest {
        runtime: Runtime::new(&cfg),
        log: Arc::new(Mutex::new(
            File::create(scratch.path().join("boot.log")).unwrap(),
        )),
    };
    let runtime = &guest.runtime;
    boot(runtime, &guest.log, &expected);
    if expected == "tcg" {
        assert_eq!(
            vm_core::accel::fallback_reason(),
            Some(vm_core::accel::AccelProblem::HypervisorConflict)
        );
    }
    guest_command(runtime, "cloud-init status --wait");
    vm_core::agentd::deploy(runtime, &agentd).unwrap();
    let build = vm_core::util::sha256_file(&agentd).unwrap();
    check_daemon(runtime, &build);

    let host = scratch.path().join("项目 sync");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("host.txt"), "host v1\n").unwrap();
    fs::write(host.join("keep.txt"), "retained\n").unwrap();
    let manager = SyncManager::open(scratch.path().join("sync"));
    let project = manager
        .create(SyncProjectInput {
            workspace_id: None,
            name: "Windows live fixture".into(),
            host_path: host.to_str().unwrap().into(),
            guest_path: "~/projects/windows-live".into(),
            mode: SyncMode::TwoWay,
            rules: SyncRules {
                git: GitHandling::Skip,
                ..SyncRules::default()
            },
        })
        .unwrap()
        .project;
    *manager.guest.write_lock() = Some(GuestTransport::Ssh(runtime.ssh_argv(false).unwrap()));
    manager
        .first_cycle(&project)
        .unwrap()
        .require_complete()
        .unwrap();
    assert_eq!(
        guest_command(runtime, "cat ~/projects/windows-live/host.txt"),
        "host v1\n"
    );
    fs::write(host.join(".gitignore"), "keep.txt\n").unwrap();
    fs::write(host.join("host.txt"), "host v2\n").unwrap();
    guest_command(
        runtime,
        "printf 'guest v1\\n' > ~/projects/windows-live/guest.txt",
    );
    manager
        .first_cycle(&project)
        .unwrap()
        .require_complete()
        .unwrap();
    assert_eq!(
        fs::read_to_string(host.join("guest.txt")).unwrap(),
        "guest v1\n"
    );
    assert_eq!(
        guest_command(
            runtime,
            "cat ~/projects/windows-live/host.txt ~/projects/windows-live/keep.txt"
        ),
        "host v2\nretained\n"
    );

    assert!(runtime.stop(&guest.log, Duration::from_secs(60)));
    assert!(!runtime.alive());
    boot(runtime, &guest.log, &expected);
    check_daemon(runtime, &build);
    guest_command(
        runtime,
        "printf 'guest v2\\n' > ~/projects/windows-live/guest.txt",
    );
    manager
        .first_cycle(&project)
        .unwrap()
        .require_complete()
        .unwrap();
    assert_eq!(
        fs::read_to_string(host.join("guest.txt")).unwrap(),
        "guest v2\n"
    );
    assert!(runtime.stop(&guest.log, Duration::from_secs(60)));
    assert!(!runtime.alive());
}
