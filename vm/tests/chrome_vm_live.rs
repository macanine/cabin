//! A disposable VM exercises in-place Chrome setup and a failed setup that puts the settings back.
//! Set CABIN_TEST_IMAGE, CABIN_TEST_ENGINE_HOME and CABIN_TEST_AGENTD.
//! Run cargo test --test chrome_vm_live -- --ignored --nocapture.
#![cfg(target_os = "macos")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vm_core::Config;
use vm_core::vm::{Boot, Runtime};

struct Scratch {
    path: PathBuf,
    cfg: Config,
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Ok(file) = std::fs::File::create(self.path.join("cleanup.log")) {
            Runtime::new(&self.cfg).stop(&Arc::new(Mutex::new(file)), Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn run(command: &mut Command) {
    let output = command.output().expect("run command");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn ws_send(stream: &mut TcpStream, payload: &[u8]) {
    assert!(payload.len() < 126);
    stream
        .write_all(&[0x82, 0x80 | payload.len() as u8, 1, 2, 3, 4])
        .unwrap();
    let masked: Vec<u8> = payload
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ [1, 2, 3, 4][i % 4])
        .collect();
    stream.write_all(&masked).unwrap();
}

struct VncStream {
    socket: TcpStream,
    pending: Vec<u8>,
}
impl VncStream {
    fn upgrade(url: &url::Url, path: &str) -> (Self, String) {
        let authority = (url.host_str().unwrap(), url.port().unwrap());
        let mut socket = TcpStream::connect(authority).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        write!(socket, "GET {path} HTTP/1.1\r\nHost: {}:{}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: MDEyMzQ1Njc4OWFiY2RlZg==\r\nSec-WebSocket-Version: 13\r\n\r\n", authority.0, authority.1).unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            header.push(byte[0]);
        }
        (
            Self {
                socket,
                pending: Vec::new(),
            },
            String::from_utf8(header).unwrap(),
        )
    }

    fn read(&mut self, count: usize) -> Vec<u8> {
        while self.pending.len() < count {
            let mut frame = [0u8; 2];
            self.socket.read_exact(&mut frame).unwrap();
            assert_eq!(frame[0] & 15, 2);
            let mut size = (frame[1] & 127) as usize;
            if size == 126 {
                let mut b = [0; 2];
                self.socket.read_exact(&mut b).unwrap();
                size = u16::from_be_bytes(b) as usize;
            } else if size == 127 {
                let mut b = [0; 8];
                self.socket.read_exact(&mut b).unwrap();
                size = u64::from_be_bytes(b) as usize;
            }
            let start = self.pending.len();
            self.pending.resize(start + size, 0);
            self.socket.read_exact(&mut self.pending[start..]).unwrap();
        }
        self.pending.drain(..count).collect()
    }
    fn key(&mut self, key: u32, down: bool) {
        let mut message = vec![4, u8::from(down), 0, 0];
        message.extend(key.to_be_bytes());
        ws_send(&mut self.socket, &message);
    }
    fn press(&mut self, key: u32) {
        self.key(key, true);
        self.key(key, false);
    }
}

#[test]
#[ignore = "boots a disposable VM and downloads Chrome and desktop packages"]
fn chrome_setup_installs_in_place_and_reverts_settings_on_failure() {
    let image = PathBuf::from(
        std::env::var_os("CABIN_TEST_IMAGE").expect("set CABIN_TEST_IMAGE"),
    );
    let engine_home = PathBuf::from(
        std::env::var_os("CABIN_TEST_ENGINE_HOME").expect("set CABIN_TEST_ENGINE_HOME"),
    );
    let agentd = PathBuf::from(
        std::env::var_os("CABIN_TEST_AGENTD").expect("set CABIN_TEST_AGENTD"),
    );
    let path = std::env::temp_dir().join(format!("chrome-live-{}", std::process::id()));
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = vm_core::config::build_config(&serde_json::json!({
        "home": path, "name": "chrometest", "username": "dev", "hostname": "chrometest",
        "network_mode": "user", "ssh_port": port, "memory_mb": 3072, "cpus": 2, "disk_size": "20G"
    }), &root).unwrap();
    std::fs::create_dir_all(cfg.instance_dir()).unwrap();
    let scratch = Scratch {
        path: path.clone(),
        cfg: cfg.clone(),
    };
    run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(cfg.ssh_key_path()));
    let key = std::fs::read_to_string(cfg.ssh_pub_path()).unwrap();
    let seed = path.join("seed-data");
    std::fs::create_dir_all(&seed).unwrap();
    std::fs::write(seed.join("user-data"), format!("#cloud-config\nusers:\n  - name: dev\n    sudo: ALL=(ALL) NOPASSWD:ALL\n    groups: [sudo]\n    shell: /bin/bash\n    ssh_authorized_keys:\n      - {}\nssh_pwauth: false\npackage_update: false\n", key.trim())).unwrap();
    std::fs::write(
        seed.join("meta-data"),
        "instance-id: chrome-live-1\nlocal-hostname: chrometest\n",
    )
    .unwrap();
    std::fs::write(
        seed.join("network-config"),
        serde_json::to_string_pretty(&vm_core::steps::seed::build_network_config(&cfg).unwrap())
            .unwrap(),
    )
    .unwrap();
    vm_core::platform::build_seed_iso(&seed, &cfg.seed_path(), &["user-data", "meta-data", "network-config"]).unwrap();
    let engine_path = vm_core::qemu_engine::root(&path);
    std::fs::create_dir_all(engine_path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(vm_core::qemu_engine::root(&engine_home), engine_path).unwrap();
    let engine = vm_core::qemu_engine::require(&path).unwrap();
    run(Command::new(&engine.qemu_img)
        .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
        .arg(image)
        .arg(cfg.disk_path()));
    run(Command::new(&engine.qemu_img)
        .arg("resize")
        .arg(cfg.disk_path())
        .arg("20G"));
    std::fs::copy(&engine.firmware_vars, cfg.efi_vars_path()).unwrap();
    cfg.save_instance_meta().unwrap();
    let log = Arc::new(Mutex::new(
        std::fs::File::create(path.join("runtime.log")).unwrap(),
    ));
    let runtime = Runtime::new(&cfg);
    runtime
        .start(&log, Boot::new(Duration::from_secs(240)))
        .unwrap();
    let (code, out) = runtime.ssh_capture("sudo -n cloud-init status --wait --long && sudo -n timedatectl set-timezone Asia/Tokyo && cp /etc/default/locale /home/dev/locale-before && printf before > /home/dev/before-chrome-test", Duration::from_secs(300));
    assert_eq!(code, 0, "{out}");

    // A failed apt install keeps the guest's data and puts the machine's
    // settings back (setup runs in place; there is no disk snapshot).
    let mut bad = cfg.clone();
    bad.desktop_extra_packages
        .push("cabin-test-package-that-does-not-exist".into());
    let error = vm_core::chrome_feature::install(&bad, &agentd)
        .unwrap_err()
        .to_string();
    assert!(error.contains("put back"), "{error}");
    let reverted = vm_core::config::load_instance_config(&root, &path, &cfg.name).unwrap();
    assert!(!reverted.desktop_chrome && reverted.desktop_env.is_none());
    let (code, out) =
        runtime.ssh_capture("cat /home/dev/before-chrome-test", Duration::from_secs(20));
    assert_eq!(code, 0, "{out}");

    vm_core::chrome_feature::install(&cfg, &agentd).unwrap_or_else(|error| {
        panic!("Chrome setup failed: {error}\nlogs in {}", path.display());
    });
    let configured = vm_core::config::load_instance_config(&root, &path, &cfg.name).unwrap();
    assert!(configured.desktop_chrome && configured.browser_enabled());
    let viewer_started = Instant::now();
    let viewer = vm_core::chrome_viewer::start(&configured).unwrap();
    let viewer_url = url::Url::parse(&viewer).unwrap();
    let (_, rejected) = VncStream::upgrade(&viewer_url, "/wrong/ws");
    assert!(rejected.contains("403 Forbidden"), "{rejected}");
    let (mut vnc, header) = VncStream::upgrade(&viewer_url, viewer_url.path());
    assert!(header.contains("101 Switching Protocols"), "{header}");
    let rfb = vnc.read(12);
    assert!(rfb.starts_with(b"RFB 003."), "{rfb:?}");
    ws_send(&mut vnc.socket, b"RFB 003.008\n");
    let count = vnc.read(1)[0] as usize;
    assert!(
        vnc.read(count).contains(&1),
        "VNC server must allow the SSH-protected local connection"
    );
    ws_send(&mut vnc.socket, &[1]);
    assert_eq!(vnc.read(4), [0, 0, 0, 0]);
    ws_send(&mut vnc.socket, &[1]);
    let init = vnc.read(24);
    let width = u16::from_be_bytes([init[0], init[1]]);
    let height = u16::from_be_bytes([init[2], init[3]]);
    assert!(width >= 640 && height >= 480, "{width}x{height}");
    let name_len = u32::from_be_bytes([init[20], init[21], init[22], init[23]]) as usize;
    let _ = vnc.read(name_len);
    // A normal VNC client negotiates encodings and requests a frame before
    // accepting input; wait for that frame so the virtual seat is ready.
    ws_send(&mut vnc.socket, &[2, 0, 0, 1, 0, 0, 0, 0]);
    let mut frame_request = vec![3, 0, 0, 0, 0, 0];
    frame_request.extend(width.to_be_bytes());
    frame_request.extend(height.to_be_bytes());
    ws_send(&mut vnc.socket, &frame_request);
    let update = vnc.read(4);
    assert_eq!(update[0], 0);
    let rectangles = u16::from_be_bytes([update[2], update[3]]) as usize;
    assert!(rectangles > 0);
    for _ in 0..rectangles {
        let rect = vnc.read(12);
        let w = u16::from_be_bytes([rect[4], rect[5]]) as usize;
        let h = u16::from_be_bytes([rect[6], rect[7]]) as usize;
        assert_eq!(&rect[8..12], &[0, 0, 0, 0]);
        let bytes_per_pixel = (init[4] as usize).div_ceil(8);
        let _ = vnc.read(w * h * bytes_per_pixel);
    }
    println!(
        "VNC first {width}×{height} frame in {:?}",
        viewer_started.elapsed()
    );
    std::thread::sleep(Duration::from_millis(500));
    // Click in the browser, focus its address bar, and navigate using VNC keys.
    let x = width / 2;
    let y = height / 2;
    ws_send(
        &mut vnc.socket,
        &[5, 1, (x >> 8) as u8, x as u8, (y >> 8) as u8, y as u8],
    );
    ws_send(
        &mut vnc.socket,
        &[5, 0, (x >> 8) as u8, x as u8, (y >> 8) as u8, y as u8],
    );
    vnc.key(0xffe3, true);
    vnc.press('l' as u32);
    vnc.key(0xffe3, false);
    for ch in "example.com".chars() {
        vnc.press(ch as u32);
    }
    vnc.press(0xff0d);
    let (code, out) = runtime.ssh_capture("for i in $(seq 1 30); do curl -fsS http://127.0.0.1:9222/json | grep -q 'example.com' && exit 0; sleep 1; done; exit 1", Duration::from_secs(40));
    assert_eq!(code, 0, "VNC keyboard did not navigate Chrome: {out}");
    vm_core::shell::browser_paste(&path, &cfg.name, Some("chrome-display-test")).unwrap();
    let (code, out) = runtime.ssh_capture(
        "uid=$(id -u); export XDG_RUNTIME_DIR=/run/user/$uid; export WAYLAND_DISPLAY=$(basename \"$(ls \"$XDG_RUNTIME_DIR\"/wayland-* | head -1)\"); wl-paste --no-newline",
        Duration::from_secs(10),
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), "chrome-display-test");
    drop(vnc);
    vm_core::agentd::deploy(&runtime, &agentd).unwrap();
    let display =
        |request| vm_core::shell::browser_native_display(&path, &cfg.name, request).unwrap();
    let baseline = display(serde_json::json!({"action": "status"}));
    let resized = display(serde_json::json!({
        "action": "apply", "width": 1440, "height": 900, "scale": 1.25, "zoom": 125
    }));
    assert_eq!(resized["width"], 1440);
    assert_eq!(resized["height"], 900);
    assert_eq!(resized["zoom"], 125);
    assert_eq!(resized["chromeWayland"], true);
    let restored = display(serde_json::json!({"action": "restore"}));
    for field in ["width", "height", "scale", "chromeWayland"] {
        assert_eq!(restored[field], baseline[field], "{field}");
    }
    let (code, out) = runtime.ssh_capture("set -eu; old=$(pgrep -u \"$(id -u)\" -x wayvnc); kill \"$old\"; for i in $(seq 1 20); do new=$(pgrep -u \"$(id -u)\" -x wayvnc || true); if [ -n \"$new\" ] && [ \"$new\" != \"$old\" ]; then exit 0; fi; sleep 0.5; done; exit 1", Duration::from_secs(15));
    assert_eq!(code, 0, "wayvnc did not recover: {out}");
    let (mut reconnected, header) = VncStream::upgrade(&viewer_url, viewer_url.path());
    assert!(header.contains("101 Switching Protocols"), "{header}");
    assert!(reconnected.read(12).starts_with(b"RFB 003."));
    drop(reconnected);
    let status = vm_core::shell::browser_status(&path, &cfg.name).unwrap();
    assert_eq!(status["viewer_ready"], true, "{status}");
    let (code, out) = runtime.ssh_capture("test \"$(cat /home/dev/before-chrome-test)\" = before && test \"$(timedatectl show -p Timezone --value)\" = Asia/Tokyo && cmp /etc/default/locale /home/dev/locale-before && google-chrome-stable --version && printf after > /home/dev/after-chrome-test", Duration::from_secs(30));
    assert_eq!(code, 0, "{out}");
    vm_core::chrome_viewer::stop(&configured).unwrap();

    vm_core::chrome_feature::enable_spice(&configured, &agentd).unwrap();
    let configured = vm_core::config::load_instance_config(&root, &path, &cfg.name).unwrap();
    assert!(configured.desktop_spice);
    let spice = vm_core::chrome_viewer::start_spice(&configured, None).unwrap();
    assert!(spice.contains("/viewer.html"));
    let resized = display(serde_json::json!({
        "action": "apply", "width": 1512, "height": 982, "scale": 1.25, "zoom": 125
    }));
    assert_eq!(resized["width"], 1512);
    assert_eq!(resized["height"], 982);
    let runtime = Runtime::new(&configured);
    runtime.stop(&log, Duration::from_secs(30));
    runtime
        .start(&log, Boot::new(Duration::from_secs(240)))
        .unwrap();
    vm_core::chrome_feature::verify_started(&configured, &agentd).unwrap();
    let state = display(serde_json::json!({"action": "status"}));
    assert_eq!(state["width"], 1512);
    assert_eq!(state["height"], 982);
    assert_eq!(state["zoom"], 125);
    let restored = display(serde_json::json!({"action": "restore"}));
    assert_eq!(restored["enabled"], false);
    assert_eq!(restored["width"], baseline["width"]);
    assert_eq!(restored["height"], baseline["height"]);
    vm_core::chrome_viewer::stop(&configured).unwrap();
    drop(scratch);
}
