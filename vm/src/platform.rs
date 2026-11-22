//! Host-platform differences: CPU acceleration, UEFI firmware locations and
//! the cloud-init seed ISO tooling.
//!
//! The product targets macOS (HVF). Linux is supported for development and
//! testing: KVM when `/dev/kvm` is usable, otherwise TCG emulation.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Result;
use crate::util::{capture, message, which};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accel {
    /// Value for `-machine virt,accel=…`.
    pub accel: &'static str,
    /// Value for `-cpu`.
    pub cpu: &'static str,
    /// Software emulation: much slower; timeouts should be generous.
    pub emulated: bool,
}

pub fn accel() -> Accel {
    if cfg!(target_os = "macos") {
        return Accel {
            accel: "hvf",
            cpu: "host",
            emulated: false,
        };
    }
    let kvm = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok();
    if kvm && std::env::consts::ARCH == "aarch64" {
        Accel {
            accel: "kvm",
            cpu: "host",
            emulated: false,
        }
    } else {
        // `pauth-impdef` replaces QARMA pointer authentication with a cheap
        // implementation-defined algorithm: a large speed-up under TCG.
        Accel {
            accel: "tcg",
            cpu: "max,pauth-impdef=on",
            emulated: true,
        }
    }
}

/// Extra time factor for guest-side waits under emulation.
pub fn slowdown() -> u32 {
    if accel().emulated { 8 } else { 1 }
}

/// Linux distribution firmware pairs (code, writable variables template).
pub fn linux_firmware_pairs() -> Vec<(PathBuf, PathBuf)> {
    [
        (
            "/usr/share/AAVMF/AAVMF_CODE.fd",
            "/usr/share/AAVMF/AAVMF_VARS.fd",
        ),
        (
            "/usr/share/qemu/edk2-aarch64-code.fd",
            "/usr/share/qemu/edk2-arm-vars.fd",
        ),
        (
            "/usr/share/edk2/aarch64/QEMU_EFI-pflash.raw",
            "/usr/share/edk2/aarch64/vars-template-pflash.raw",
        ),
    ]
    .iter()
    .map(|(code, vars)| (PathBuf::from(code), PathBuf::from(vars)))
    .collect()
}

/// The distribution's QEMU on a Linux development host (macOS always uses
/// the built-in engine, see `qemu_engine`).
pub fn linux_qemu() -> Option<crate::qemu_engine::Engine> {
    let qemu = which("qemu-system-aarch64")?;
    let qemu_img = which("qemu-img")?;
    let (code, vars) = linux_firmware_pairs()
        .into_iter()
        .find(|(code, vars)| code.is_file() && vars.is_file())?;
    Some(crate::qemu_engine::Engine {
        qemu,
        qemu_img,
        firmware_dir: code.parent().map(Path::to_path_buf).unwrap_or_default(),
        firmware_code: code,
        firmware_vars: vars,
    })
}

/// Tools the VM pipeline needs on this host.
pub fn required_tools() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["ssh", "ssh-keygen", "hdiutil", "openssl"]
    } else {
        &["ssh", "ssh-keygen", "xorriso", "openssl"]
    }
}

/// Build a CIDATA ISO from the files in `stage` (macOS: hdiutil, Linux: xorriso).
pub fn seed_iso_argv(stage: &Path, output: &Path, files: &[&str]) -> Vec<String> {
    if cfg!(target_os = "macos") {
        vec![
            "hdiutil".into(),
            "makehybrid".into(),
            "-o".into(),
            output.display().to_string(),
            "-iso".into(),
            "-joliet".into(),
            "-default-volume-name".into(),
            "CIDATA".into(),
            stage.display().to_string(),
        ]
    } else {
        let mut argv: Vec<String> = [
            "xorriso", "-as", "mkisofs", "-quiet", "-volid", "CIDATA", "-joliet", "-rock",
            "-output",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        argv.push(output.display().to_string());
        for file in files {
            argv.push(stage.join(file).display().to_string());
        }
        argv
    }
}

/// Linux: check the ISO's volume label and extract it (no mount needed).
pub fn linux_extract_iso(iso: &Path, into: &Path) -> Result<String> {
    if which("xorriso").is_none() {
        return message("xorriso is required to verify the seed ISO");
    }
    let info = capture(
        &[
            "xorriso".into(),
            "-indev".into(),
            iso.display().to_string(),
            "-pvd_info".into(),
        ],
        Duration::from_secs(30),
        None,
    );
    let label = info
        .output
        .lines()
        .find_map(|line| line.strip_prefix("Volume Id    :"))
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    let extract = capture(
        &[
            "xorriso".into(),
            "-osirrox".into(),
            "on".into(),
            "-indev".into(),
            iso.display().to_string(),
            "-extract".into(),
            "/".into(),
            into.display().to_string(),
        ],
        Duration::from_secs(60),
        None,
    );
    if extract.code != 0 {
        return message(format!(
            "cannot read generated seed ISO: {}",
            extract.output.trim()
        ));
    }
    Ok(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_iso_round_trip_on_linux() {
        if cfg!(target_os = "macos") || which("xorriso").is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("seed-iso-test-{}", std::process::id()));
        let stage = dir.join("stage");
        std::fs::create_dir_all(&stage).unwrap();
        for name in ["user-data", "meta-data", "network-config"] {
            std::fs::write(stage.join(name), format!("{name}\n")).unwrap();
        }
        let iso = dir.join("seed.iso");
        let argv = seed_iso_argv(&stage, &iso, &["user-data", "meta-data", "network-config"]);
        let built = capture(&argv, Duration::from_secs(30), None);
        assert_eq!(built.code, 0, "{}", built.output);
        let label = linux_extract_iso(&iso, &dir.join("out")).unwrap();
        assert_eq!(label.to_lowercase(), "cidata");
        assert_eq!(
            std::fs::read_to_string(dir.join("out/user-data")).unwrap(),
            "user-data\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
