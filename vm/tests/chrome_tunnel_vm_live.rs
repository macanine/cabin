//! Fast opt-in check of the WebSocket → SSH → guest loopback tunnel.
//! Set CABIN_TEST_IMAGE and CABIN_TEST_ENGINE_HOME.
#![cfg(target_os = "macos")]
mod support;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vm_core::{
    Config,
    vm::{Boot, Runtime},
};

struct Guest {
    cfg: Config,
    dir: PathBuf,
}
impl Drop for Guest {
    fn drop(&mut self) {
        if let Ok(f) = std::fs::File::create(self.dir.join("stop.log")) {
            Runtime::new(&self.cfg).stop(&Arc::new(Mutex::new(f)), Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn run(cmd: &mut Command) {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "boots an isolated guest to test the Chrome display tunnel"]
fn websocket_bridges_to_guest_loopback_over_ssh() {
    let image = PathBuf::from(std::env::var_os("CABIN_TEST_IMAGE").unwrap());
    let engine_home = PathBuf::from(std::env::var_os("CABIN_TEST_ENGINE_HOME").unwrap());
    let dir = PathBuf::from(format!("/tmp/va-tunnel-{}", std::process::id()));
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = vm_core::build_config(&serde_json::json!({"home":dir,"name":"tunneltest","username":"dev","hostname":"tunneltest","network_mode":"user","ssh_port":port,"memory_mb":1024,"cpus":2,"disk_size":"10G"}), &root).unwrap();
    std::fs::create_dir_all(cfg.instance_dir()).unwrap();
    let _guest = Guest {
        cfg: cfg.clone(),
        dir: dir.clone(),
    };
    run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(cfg.ssh_key_path()));
    let key = std::fs::read_to_string(cfg.ssh_pub_path()).unwrap();
    let seed = dir.join("seed-data");
    std::fs::create_dir_all(&seed).unwrap();
    std::fs::write(seed.join("user-data"), format!("#cloud-config\nusers:\n  - name: dev\n    sudo: ALL=(ALL) NOPASSWD:ALL\n    groups: [sudo]\n    shell: /bin/bash\n    ssh_authorized_keys:\n      - {}\nssh_pwauth: false\npackage_update: false\n", key.trim())).unwrap();
    std::fs::write(
        seed.join("meta-data"),
        "instance-id: tunnel-test-1\nlocal-hostname: tunneltest\n",
    )
    .unwrap();
    std::fs::write(
        seed.join("network-config"),
        serde_json::to_string_pretty(&vm_core::steps::seed::build_network_config(&cfg).unwrap())
            .unwrap(),
    )
    .unwrap();
    run(Command::new("hdiutil")
        .args([
            "makehybrid",
            "-quiet",
            "-iso",
            "-joliet",
            "-default-volume-name",
            "cidata",
            "-o",
        ])
        .arg(cfg.seed_path())
        .arg(&seed));
    let engine_path = vm_core::qemu_engine::root(&dir);
    std::fs::create_dir_all(engine_path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(vm_core::qemu_engine::root(&engine_home), engine_path).unwrap();
    let engine = vm_core::qemu_engine::require(&dir).unwrap();
    run(Command::new(&engine.qemu_img)
        .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
        .arg(image)
        .arg(cfg.disk_path()));
    run(Command::new(&engine.qemu_img)
        .arg("resize")
        .arg(cfg.disk_path())
        .arg("10G"));
    std::fs::copy(engine.firmware_vars, cfg.efi_vars_path()).unwrap();
    let log = Arc::new(Mutex::new(
        std::fs::File::create(dir.join("runtime.log")).unwrap(),
    ));
    let runtime = Runtime::new(&cfg);
    runtime
        .start(&log, Boot::new(Duration::from_secs(240)))
        .unwrap();
    let helper_dir = dir.join("test-helper");
    std::fs::create_dir_all(&helper_dir).unwrap();
    let helper = support::build_guest_helper(&helper_dir);
    let ssh = runtime.ssh_argv(false).unwrap();
    support::install_guest_helper(Command::new(&ssh[0]).args(&ssh[1..]), &helper);
    let (code, out) = runtime.ssh_capture("cloud-init status --wait >/dev/null; nohup \"$HOME/.local/bin/guest-test-helper\" vnc >/tmp/fake-vnc.log 2>&1 </dev/null &", Duration::from_secs(120));
    assert_eq!(code, 0, "{out}");
    let (code, out) = runtime.ssh_capture("for i in $(seq 1 20); do \"$HOME/.local/bin/guest-test-helper\" network connect 127.0.0.1:5900 >/dev/null && exit 0; sleep 1; done; cat /tmp/fake-vnc.log; exit 1", Duration::from_secs(30));
    assert_eq!(code, 0, "{out}");
    let mut ssh = runtime.ssh_argv(false).unwrap();
    let target = ssh.pop().unwrap();
    ssh.extend(["-T".into(), "-W".into(), "127.0.0.1:5900".into(), target]);
    let direct = Command::new(&ssh[0]).args(&ssh[1..]).output().unwrap();
    assert!(
        direct.stdout.starts_with(b"RFB 003."),
        "direct SSH: status={} stdout={:?} stderr={}",
        direct.status,
        direct.stdout,
        String::from_utf8_lossy(&direct.stderr)
    );
    let viewer = vm_core::chrome_viewer::start(&cfg).unwrap();
    let url = url::Url::parse(&viewer).unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", url.port().unwrap())).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(stream,"GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: MDEyMzQ1Njc4OWFiY2RlZg==\r\nSec-WebSocket-Version: 13\r\n\r\n",url.path(),url.port().unwrap()).unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut b = [0];
        stream.read_exact(&mut b).unwrap();
        header.push(b[0]);
    }
    let mut frame = [0; 2];
    if let Err(error) = stream.read_exact(&mut frame) {
        panic!(
            "no VNC response: {error}; {}",
            std::fs::read_to_string(cfg.instance_logs_dir().join("chrome-viewer.log"))
                .unwrap_or_default()
        );
    }
    let mut payload = vec![0; (frame[1] & 127) as usize];
    stream.read_exact(&mut payload).unwrap();
    assert!(payload.starts_with(b"RFB 003."), "{payload:?}");
    vm_core::chrome_viewer::stop(&cfg).unwrap();
}
