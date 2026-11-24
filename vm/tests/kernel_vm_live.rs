//! Opt-in first-boot and reboot test against a disposable Ubuntu cloud image.
//! CABIN_TEST_IMAGE=/path/to/ubuntu-arm64.img cargo test --test kernel_vm_live -- --ignored --nocapture
#![cfg(target_os = "macos")]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Vm(Child);
impl Drop for Vm {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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

fn ssh(port: u16, key: &Path, command: &str) -> Output {
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
            "IdentitiesOnly=yes",
            "-o",
            "ConnectTimeout=3",
            "-o",
            "LogLevel=ERROR",
            "-i",
        ])
        .arg(key)
        .args(["-p", &port.to_string(), "nettest@127.0.0.1", command])
        .output()
        .expect("run ssh")
}

const CHECK_PACKAGES: &str = r#"set -eu
status() { dpkg-query -W -f='${db:Status-Status}' "$1" 2>/dev/null || true; }
[ "$(status linux-image-generic)" = installed ]
apt-mark showmanual linux-image-generic | grep -Fxq linux-image-generic
[ "$(status linux-image-virtual)" != installed ]
[ "$(status linux-virtual)" != installed ]
running=$(uname -r)
[ "$(status "linux-image-$running")" = installed ]
[ "$(status "linux-modules-extra-$running")" = installed ]
dpkg-query -W -f='${Depends}' linux-image-generic | grep -Fq "linux-image-$running"
printf 'running=%s\n' "$running"
"#;

#[test]
#[ignore = "boots a disposable QEMU VM and installs packages from Ubuntu archives"]
fn first_boot_replaces_virtual_kernel_metapackage_and_boots_generic() {
    let image = PathBuf::from(
        std::env::var_os("CABIN_TEST_IMAGE").expect("set CABIN_TEST_IMAGE"),
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = vm_core::config::build_config(
        &serde_json::json!({"name": "kernel-test", "username": "nettest", "hostname": "kernel-test", "network_mode": "user"}),
        &root,
    ).unwrap();
    let temp =
        Scratch(std::env::temp_dir().join(format!("cabin-kernel-live-{}", std::process::id())));
    std::fs::create_dir_all(temp.0.join("seed")).unwrap();
    let key = temp.0.join("key");
    run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key));
    let public_key = std::fs::read_to_string(temp.0.join("key.pub")).unwrap();
    let user_data = vm_core::steps::seed::build_user_data(&cfg, "!", public_key.trim()).unwrap();
    std::fs::write(
        temp.0.join("seed/user-data"),
        format!(
            "#cloud-config\n{}\n",
            serde_json::to_string_pretty(&user_data).unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        temp.0.join("seed/meta-data"),
        "instance-id: kernel-test-1\nlocal-hostname: kernel-test\n",
    )
    .unwrap();
    let network = vm_core::steps::seed::build_network_config(&cfg).unwrap();
    std::fs::write(
        temp.0.join("seed/network-config"),
        serde_json::to_string_pretty(&network).unwrap(),
    )
    .unwrap();

    let iso = temp.0.join("seed.iso");
    vm_core::platform::build_seed_iso(&temp.0.join("seed"), &iso, &["user-data", "meta-data", "network-config"]).unwrap();
    let disk = temp.0.join("disk.qcow2");
    run(Command::new("qemu-img")
        .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
        .arg(&image)
        .arg(&disk));
    run(Command::new("qemu-img").arg("resize").arg(&disk).arg("20G"));
    std::fs::copy(
        "/opt/homebrew/share/qemu/edk2-arm-vars.fd",
        temp.0.join("vars.fd"),
    )
    .unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let log = std::fs::File::create(temp.0.join("serial.log")).unwrap();
    let child = Command::new("qemu-system-aarch64")
        .args(["-machine", "virt,accel=hvf", "-cpu", "host", "-m", "2048", "-smp", "2", "-display", "none", "-monitor", "none", "-serial", "stdio"])
        .args(["-drive", "if=pflash,format=raw,unit=0,readonly=on,file=/opt/homebrew/share/qemu/edk2-aarch64-code.fd"])
        .arg("-drive").arg(format!("if=pflash,format=raw,unit=1,file={}", temp.0.join("vars.fd").display()))
        .arg("-drive").arg(format!("if=virtio,format=qcow2,file={}", disk.display()))
        .arg("-drive").arg(format!("if=virtio,format=raw,readonly=on,file={}", iso.display()))
        .args(["-netdev", &format!("user,id=net,ipv6=off,hostfwd=tcp:127.0.0.1:{port}-:22"), "-device", &format!("virtio-net-pci,netdev=net,mac={}", cfg.mac_mgmt())])
        .args(["-device", "virtio-serial-pci", "-chardev", &format!("socket,id=qga0,path={},server=on,wait=off", temp.0.join("qga.sock").display()), "-device", "virtserialport,chardev=qga0,name=org.qemu.guest_agent.0"])
        .stdout(Stdio::from(log)).stderr(Stdio::null()).spawn().unwrap();
    let _vm = Vm(child);

    let deadline = Instant::now() + Duration::from_secs(180);
    while !ssh(port, &key, "true").status.success() {
        assert!(
            Instant::now() < deadline,
            "VM did not start: {}",
            std::fs::read_to_string(temp.0.join("serial.log")).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_secs(2));
    }
    println!("Disposable guest is running; waiting for cloud-init and kernel packages");
    let setup = ssh(port, &key, "timeout 900 cloud-init status --wait --long");
    assert!(
        setup.status.success(),
        "cloud-init failed: {} {}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr)
    );
    let boot_id = ssh(port, &key, "cat /proc/sys/kernel/random/boot_id");
    assert!(boot_id.status.success());
    let previous_boot = String::from_utf8_lossy(&boot_id.stdout).trim().to_string();
    let _ = ssh(port, &key, "sudo -n reboot");
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let boot = ssh(port, &key, "cat /proc/sys/kernel/random/boot_id");
        if boot.status.success() && String::from_utf8_lossy(&boot.stdout).trim() != previous_boot {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "VM did not reboot: {}",
            std::fs::read_to_string(temp.0.join("serial.log")).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_secs(2));
    }
    let check = ssh(port, &key, CHECK_PACKAGES);
    assert!(
        check.status.success(),
        "kernel check failed: {} {}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    println!(
        "Generic kernel booted after replacement: {}",
        String::from_utf8_lossy(&check.stdout).trim()
    );
}

#[test]
#[ignore = "boots an isolated ARM64 Ubuntu VM twice; requires CABIN_TEST_IMAGE and CABIND_TEST_BIN"]
fn expands_an_existing_ubuntu_root_without_cloud_init_or_data_loss() {
    let image = PathBuf::from(std::env::var_os("CABIN_TEST_IMAGE").expect("set CABIN_TEST_IMAGE"));
    let helper = PathBuf::from(std::env::var_os("CABIND_TEST_BIN").expect("set CABIND_TEST_BIN"));
    let temp = Scratch(std::env::temp_dir().join(format!("cabin-grow-root-live-{}", std::process::id())));
    std::fs::create_dir(&temp.0).unwrap();
    let key = temp.0.join("key");
    run(Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(&key));
    let public = std::fs::read_to_string(temp.0.join("key.pub")).unwrap();
    let user_data = format!(r#"#cloud-config
users:
  - name: nettest
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    lock_passwd: true
    ssh_authorized_keys: [{}]
    "#, serde_json::to_string(public.trim()).unwrap());
    let iso = temp.0.join("seed.iso");
    vm_core::iso::build_iso(&iso, &[
        ("user-data", user_data.as_bytes()),
        ("meta-data", b"instance-id: cabin-grow-test\nlocal-hostname: cabin-grow-test\n"),
    ]).unwrap();
    let disk = temp.0.join("disk.qcow2");
    run(Command::new("qemu-img").args(["convert", "-O", "qcow2"]).arg(&image).arg(&disk));
    run(Command::new("qemu-img").arg("resize").arg(&disk).arg("6G"));
    let vars = temp.0.join("vars.fd");
    std::fs::copy("/opt/homebrew/share/qemu/edk2-arm-vars.fd", &vars).unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();

    let start = |cpus: &str, memory: &str| {
        let log_path = temp.0.join("serial.log");
        let log = std::fs::File::create(&log_path).unwrap();
        let child = Command::new("qemu-system-aarch64")
            .args(["-machine", "virt,accel=hvf", "-cpu", "host", "-m", memory, "-smp", cpus, "-display", "none", "-monitor", "none", "-serial", "stdio"])
            .args(["-drive", "if=pflash,format=raw,unit=0,readonly=on,file=/opt/homebrew/share/qemu/edk2-aarch64-code.fd"])
            .arg("-drive").arg(format!("if=pflash,format=raw,unit=1,file={}", vars.display()))
            .arg("-drive").arg(format!("if=virtio,format=qcow2,file={}", disk.display()))
            .arg("-drive").arg(format!("if=virtio,format=raw,readonly=on,file={}", iso.display()))
            .args(["-netdev", &format!("user,id=net,ipv6=off,hostfwd=tcp:127.0.0.1:{port}-:22"), "-device", "virtio-net-pci,netdev=net"])
            .stdout(Stdio::from(log.try_clone().unwrap())).stderr(Stdio::from(log)).spawn().unwrap();
        let vm = Vm(child);
        let deadline = Instant::now() + Duration::from_secs(120);
        while !ssh(port, &key, "true").status.success() {
            assert!(Instant::now() < deadline, "{}", std::fs::read_to_string(&log_path).unwrap_or_default());
            std::thread::sleep(Duration::from_secs(1));
        }
        vm
    };
    let probe = |command: &str| {
        let output = ssh(port, &key, command);
        assert!(output.status.success(), "{command}: {} {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    let stop = |mut vm: Vm| {
        let _ = ssh(port, &key, "sudo -n poweroff");
        let deadline = Instant::now() + Duration::from_secs(30);
        while vm.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "test guest did not power off");
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    let root_bytes = || probe("findmnt --bytes --noheadings --output SIZE --mountpoint /").parse::<u64>().unwrap();

    let vm = start("1", "1024");
    probe("timeout 90 cloud-init status --wait && printf cabin-sentinel > ~/disk-sentinel && sudo touch /etc/cloud/cloud-init.disabled");
    let before = root_bytes();
    assert_eq!(probe("nproc"), "1");
    stop(vm);

    run(Command::new("qemu-img").arg("resize").arg(&disk).arg("8G"));
    let vm = start("2", "2048");
    run(Command::new("scp").args(["-q", "-F", "/dev/null", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-i"])
        .arg(&key).args(["-P", &port.to_string()]).arg(&helper).arg("nettest@127.0.0.1:/tmp/cabind-grow-test"));
    assert_eq!(probe("nproc"), "2");
    assert_eq!(root_bytes(), before, "cloud-init should not have expanded the root");
    let grow = "chmod 0755 /tmp/cabind-grow-test && sudo -n /tmp/cabind-grow-test guest-system grow-root 8589934592";
    println!("{}", probe(grow));
    let after = root_bytes();
    assert!(after > before + 1024 * 1024 * 1024, "root filesystem did not grow: {before} -> {after}");
    assert_eq!(probe("cat ~/disk-sentinel"), "cabin-sentinel");
    probe(grow);
    assert_eq!(root_bytes(), after);
    println!("Guest root filesystem grew from {before} to {after} bytes; data and retry verified");
    stop(vm);
}
