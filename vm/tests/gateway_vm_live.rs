//! Opt-in end-to-end test using an isolated Ubuntu cloud image and QEMU.
//! CABIN_TEST_IMAGE=/path/to/ubuntu-arm64.img cargo test --test gateway_vm_live -- --ignored --nocapture
#![cfg(target_os = "macos")]
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use vm_core::netgw::{GUEST_MAC, Gateway, Upstream};

mod support;

struct Vm(Child);
impl Drop for Vm {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(cmd: &mut Command) {
    let output = cmd.output().expect("run command");
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn ssh(port: u16, key: &Path, command: &str) -> std::process::Output {
    Command::new("ssh")
        .args([
            "-F",
            "/dev/null",
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=3",
            "-o",
            "LogLevel=ERROR",
            "-i",
        ])
        .arg(key)
        .args(["-p", &port.to_string(), "nettest@127.0.0.1", command])
        .output()
        .unwrap()
}

#[test]
#[ignore = "starts an isolated QEMU VM and accesses the network"]
fn guest_dns_tcp_proxy_switch_and_fail_closed() {
    let image = PathBuf::from(std::env::var_os("CABIN_TEST_IMAGE").expect("set CABIN_TEST_IMAGE"));
    let temp = std::env::temp_dir().join(format!("cabin-gw-{}", std::process::id()));
    std::fs::create_dir_all(temp.join("seed")).unwrap();
    let key = temp.join("key");
    run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key));
    let public_key = std::fs::read_to_string(temp.join("key.pub")).unwrap();
    std::fs::write(temp.join("seed/user-data"), format!("#cloud-config\nusers:\n  - name: nettest\n    sudo: ALL=(ALL) NOPASSWD:ALL\n    groups: [sudo]\n    shell: /bin/bash\n    ssh_authorized_keys:\n      - {}\nssh_pwauth: false\npackage_update: false\n", public_key.trim())).unwrap();
    std::fs::write(
        temp.join("seed/meta-data"),
        "instance-id: network-test-1\nlocal-hostname: network-test\n",
    )
    .unwrap();
    let data_mac = GUEST_MAC
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":");
    std::fs::write(temp.join("seed/network-config"), format!("version: 2\nethernets:\n  mgmt0:\n    match: {{macaddress: '52:54:00:12:34:56'}}\n    set-name: mgmt0\n    dhcp4: true\n    dhcp4-overrides: {{use-routes: false, use-dns: false}}\n    optional: true\n  data0:\n    match: {{macaddress: '{data_mac}'}}\n    set-name: data0\n    dhcp4: true\n")).unwrap();
    let iso = temp.join("seed.iso");
    vm_core::platform::build_seed_iso(
        &temp.join("seed"),
        &iso,
        &["user-data", "meta-data", "network-config"],
    )
    .unwrap();
    let disk = temp.join("disk.qcow2");
    run(Command::new("qemu-img")
        .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
        .arg(&image)
        .arg(&disk));
    std::fs::copy(
        "/opt/homebrew/share/qemu/edk2-arm-vars.fd",
        temp.join("vars.fd"),
    )
    .unwrap();
    let socket = temp.join("net.sock");
    let gateway = Gateway::start(
        &socket,
        vec![Ipv4Addr::new(1, 1, 1, 1)],
        &Upstream::Direct,
        (Ipv4Addr::LOCALHOST, 0).into(),
    )
    .unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let log = std::fs::File::create(temp.join("serial.log")).unwrap();
    let child = Command::new("qemu-system-aarch64").args(["-machine", "virt,accel=hvf", "-cpu", "host", "-m", "1024", "-smp", "2", "-display", "none", "-monitor", "none", "-serial", "stdio", "-drive", "if=pflash,format=raw,unit=0,readonly=on,file=/opt/homebrew/share/qemu/edk2-aarch64-code.fd", "-drive"]).arg(format!("if=pflash,format=raw,unit=1,file={}", temp.join("vars.fd").display())).arg("-drive").arg(format!("if=virtio,format=qcow2,file={}", disk.display())).arg("-drive").arg(format!("if=virtio,format=raw,readonly=on,file={}", iso.display())).args(["-netdev", &format!("user,id=mgmt,restrict=on,ipv6=off,hostfwd=tcp:127.0.0.1:{port}-:22"), "-device", "virtio-net-pci,netdev=mgmt,mac=52:54:00:12:34:56", "-netdev", &format!("stream,id=data,server=off,addr.type=unix,addr.path={},reconnect-ms=1000", socket.display()), "-device", &format!("virtio-net-pci,netdev=data,mac={data_mac}")]).stdout(Stdio::from(log)).stderr(Stdio::null()).spawn().unwrap();
    let _vm = Vm(child);
    let started = Instant::now();
    loop {
        let output = ssh(port, &key, "true");
        if output.status.success() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(150),
            "VM did not start; serial log: {}",
            temp.join("serial.log").display()
        );
        std::thread::sleep(Duration::from_secs(2));
    }
    let helper_dir = temp.join("helper");
    std::fs::create_dir_all(&helper_dir).unwrap();
    let helper = support::build_guest_helper(&helper_dir);
    let mut install = Command::new("ssh");
    install
        .args([
            "-F",
            "none",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "LogLevel=ERROR",
            "-i",
        ])
        .arg(&key)
        .args(["-p", &port.to_string(), "nettest@127.0.0.1"]);
    support::install_guest_helper(&mut install, &helper);
    let output = ssh(
        port,
        &key,
        r#""$HOME/.local/bin/guest-test-helper" network resolve example.com"#,
    );
    assert!(
        output.status.success(),
        "DNS failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    println!(
        "Guest DNS: {}",
        String::from_utf8_lossy(&output.stdout).trim()
    );
    let output = ssh(
        port,
        &key,
        r#""$HOME/.local/bin/guest-test-helper" network http https://example.com"#,
    );
    assert!(
        output.status.success(),
        "direct HTTPS failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    println!(
        "Guest direct HTTPS: {}",
        String::from_utf8_lossy(&output.stdout).trim()
    );
    let output = ssh(
        port,
        &key,
        r#""$HOME/.local/bin/guest-test-helper" network connect 10.0.2.2:22",
    );
    assert!(
        !output.status.success(),
        "the management NIC reached the host"
    );

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let socks_addr = listener.local_addr().unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let server_stop = stop.clone();
    let server_requests = requests.clone();
    let server = std::thread::spawn(move || {
        while !server_stop.load(std::sync::atomic::Ordering::Relaxed) {
            let mut stream = match listener.accept() {
                Ok((s, _)) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                Err(e) => panic!("SOCKS accept: {e}"),
            };
            let requests = server_requests.clone();
            std::thread::spawn(move || {
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut greeting = [0u8; 2];
                stream.read_exact(&mut greeting).unwrap();
                let mut methods = vec![0; greeting[1] as usize];
                stream.read_exact(&mut methods).unwrap();
                let mut header = [0u8; 4];
                stream.read_exact(&mut header).unwrap();
                assert_eq!(&header[..2], &[5, 1]);
                let count = match header[3] {
                    1 => 4,
                    4 => 16,
                    3 => {
                        let mut n = [0];
                        stream.read_exact(&mut n).unwrap();
                        n[0] as usize
                    }
                    _ => panic!("bad address"),
                };
                let mut address = vec![0; count + 2];
                stream.read_exact(&mut address).unwrap();
                requests.lock().unwrap().push(address.clone());
                let is_test = address == [1, 1, 1, 1, 0, 80];
                let host = match header[3] {
                    1 => Ipv4Addr::new(address[0], address[1], address[2], address[3]).to_string(),
                    3 => String::from_utf8(address[..count].to_vec()).unwrap(),
                    4 => std::net::Ipv6Addr::from(<[u8; 16]>::try_from(&address[..16]).unwrap())
                        .to_string(),
                    _ => unreachable!(),
                };
                let target_port = u16::from_be_bytes([address[count], address[count + 1]]);
                let upstream = if is_test {
                    None
                } else {
                    Some(std::net::TcpStream::connect((host.as_str(), target_port)).unwrap())
                };
                stream
                    .write_all(&[5, 0, 5, 0, 0, 1, 127, 0, 0, 1, 0, 1])
                    .unwrap();
                if let Some(mut upstream) = upstream {
                    let mut stream_in = stream.try_clone().unwrap();
                    let mut upstream_out = upstream.try_clone().unwrap();
                    std::thread::spawn(move || {
                        let _ = std::io::copy(&mut stream_in, &mut upstream_out);
                    });
                    let _ = std::io::copy(&mut upstream, &mut stream);
                    return;
                }
                let mut request = [0u8; 1024];
                let len = stream.read(&mut request).unwrap();
                assert!(request[..len].starts_with(b"GET /"));
                stream
                    .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                    .unwrap();
            });
        }
    });
    let hop: serde_yaml::Value = serde_yaml::from_str(&format!(
        "address: '{socks_addr}'\nprotocol:\n  type: socks\n"
    ))
    .unwrap();
    gateway.set_upstream(&Upstream::Chain(vec![hop])).unwrap();
    let output = ssh(
        port,
        &key,
        r#""$HOME/.local/bin/guest-test-helper" network connect 1.1.1.1:80 "GET / HTTP/1.0"",
    );
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("200 OK"),
        "proxy request failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .any(|address| address == &[1, 1, 1, 1, 0, 80])
    );
    println!("Guest TCP reached the selected SOCKS node");
    let output = ssh(
        port,
        &key,
        r#""$HOME/.local/bin/guest-test-helper" network resolve www.rust-lang.org",
    );
    assert!(
        output.status.success(),
        "DNS through proxy failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .any(|address| address.ends_with(&443u16.to_be_bytes()))
    );
    println!(
        "Guest DNS reached the selected SOCKS node: {}",
        String::from_utf8_lossy(&output.stdout).trim()
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    server.join().unwrap();
    let dead: serde_yaml::Value =
        serde_yaml::from_str("address: '127.0.0.1:1'\nprotocol:\n  type: socks\n").unwrap();
    let dead = Upstream::Chain(vec![dead]);
    gateway.set_upstream(&dead).unwrap();
    let output = ssh(
        port,
        &key,
        r#""$HOME/.local/bin/guest-test-helper" network connect 1.1.1.1:80 "GET / HTTP/1.0"",
    );
    assert!(
        !output.status.success(),
        "traffic bypassed the failed upstream"
    );
    println!("Failed upstream did not fall back to direct");
    drop(gateway);
    let gateway = Gateway::start(
        &socket,
        vec![Ipv4Addr::new(1, 1, 1, 1)],
        &dead,
        (Ipv4Addr::LOCALHOST, 0).into(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !gateway.connected() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        gateway.connected(),
        "QEMU did not reconnect to the restarted gateway"
    );
    let output = ssh(
        port,
        &key,
        r#""$HOME/.local/bin/guest-test-helper" network connect 1.1.1.1:80 "GET / HTTP/1.0"",
    );
    assert!(
        !output.status.success(),
        "restarting the gateway bypassed the failed upstream"
    );
    gateway.set_upstream(&Upstream::Direct).unwrap();
    let output = ssh(
        port,
        &key,
        r#""$HOME/.local/bin/guest-test-helper" network http https://example.com"#,
    );
    assert!(
        output.status.success(),
        "direct mode did not recover after restart: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    println!("Gateway restarted, retained fail-closed routing and recovered in direct mode");
    drop(_vm);
    drop(gateway);
    let _ = std::fs::remove_dir_all(temp);
}
