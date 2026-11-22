//! Host-platform differences: CPU acceleration, UEFI firmware locations and
//! the cloud-init seed ISO tooling.
//!
//! The product targets macOS (HVF). Linux is supported for development and
//! testing: KVM when `/dev/kvm` is usable, otherwise TCG emulation. Windows
//! uses a native-architecture guest and QEMU from PATH or Program Files.
//! It builds the seed ISO in-process (`iso`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Result;
use crate::util::{capture, message, which};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accel {
    /// Value for `-machine …,accel=…`.
    pub accel: &'static str,
    /// Value for `-cpu` (empty: QEMU's default, e.g. under WHPX).
    pub cpu: &'static str,
    /// Software emulation: much slower; timeouts should be generous.
    pub emulated: bool,
}

/// The architecture of the Ubuntu guest this host runs. The macOS product
/// keeps its Apple Silicon guest; other hosts run their native architecture
/// so hardware acceleration (KVM/WHPX) stays possible.
pub fn guest_arch() -> &'static str {
    if cfg!(target_os = "macos") {
        "aarch64"
    } else {
        std::env::consts::ARCH
    }
}

/// Whether the Windows Hypervisor Platform feature is enabled (WHPX needs it).
#[cfg(windows)]
fn whpx_enabled() -> bool {
    std::env::var_os("SystemRoot")
        .map(|root| {
            Path::new(&root)
                .join("System32")
                .join("WindowsHypervisorPlatform.dll")
                .is_file()
        })
        .unwrap_or(false)
}

pub fn accel() -> Accel {
    if cfg!(target_os = "macos") {
        return Accel {
            accel: "hvf",
            cpu: "host",
            emulated: false,
        };
    }
    let arm = guest_arch() == "aarch64";
    #[cfg(windows)]
    {
        if !arm && whpx_enabled() {
            return Accel {
                accel: "whpx",
                // WHPX has no host CPU passthrough model; QEMU's default is
                // the safe choice (adding features it cannot virtualize
                // fails the launch).
                cpu: "",
                emulated: false,
            };
        }
        return Accel {
            accel: "tcg",
            cpu: if arm {
                "max,pauth-impdef=on"
            } else {
                "max"
            },
            emulated: true,
        };
    }
    #[cfg(not(windows))]
    {
        let kvm = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")
            .is_ok();
        if kvm {
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
                cpu: if arm {
                    "max,pauth-impdef=on"
                } else {
                    "max"
                },
                emulated: true,
            }
        }
    }
}

/// Extra time factor for guest-side waits under emulation.
pub fn slowdown() -> u32 {
    if accel().emulated { 8 } else { 1 }
}

/// The QEMU binary name for this host's guest architecture.
pub fn qemu_system() -> &'static str {
    if guest_arch() == "x86_64" {
        "qemu-system-x86_64"
    } else {
        "qemu-system-aarch64"
    }
}

/// Linux distribution firmware pairs (code, writable variables template).
#[cfg(unix)]
pub fn distro_firmware_pairs() -> Vec<(PathBuf, PathBuf)> {
    let pairs: [(&str, &str); 3] = if guest_arch() == "x86_64" {
        [
            ("/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/OVMF/OVMF_VARS.fd"),
            (
                "/usr/share/qemu/edk2-x86_64-code.fd",
                "/usr/share/qemu/edk2-i386-vars.fd",
            ),
            (
                "/usr/share/OVMF/OVMF_CODE_4M.fd",
                "/usr/share/OVMF/OVMF_VARS_4M.fd",
            ),
        ]
    } else {
        [
            ("/usr/share/AAVMF/AAVMF_CODE.fd", "/usr/share/AAVMF/AAVMF_VARS.fd"),
            (
                "/usr/share/qemu/edk2-aarch64-code.fd",
                "/usr/share/qemu/edk2-arm-vars.fd",
            ),
            (
                "/usr/share/edk2/aarch64/QEMU_EFI-pflash.raw",
                "/usr/share/edk2/aarch64/vars-template-pflash.raw",
            ),
        ]
    };
    pairs
        .iter()
        .map(|(code, vars)| (PathBuf::from(code), PathBuf::from(vars)))
        .collect()
}

/// Windows firmware pairs: the qemu.org installer ships the EDK2 images next
/// to the binaries (`share\edk2-*.fd` on current installs; older layouts
/// kept a `share\qemu` subfolder).
#[cfg(windows)]
pub fn distro_firmware_pairs() -> Vec<(PathBuf, PathBuf)> {
    let (code_name, vars_name) = if guest_arch() == "x86_64" {
        ("edk2-x86_64-code.fd", "edk2-i386-vars.fd")
    } else {
        ("edk2-aarch64-code.fd", "edk2-arm-vars.fd")
    };
    let mut roots = Vec::new();
    if let Some(qemu) = windows_qemu_tool(qemu_system())
        && let Some(parent) = qemu.parent()
    {
        roots.push(parent.to_path_buf());
        roots.extend(parent.parent().map(Path::to_path_buf));
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(value) = std::env::var_os(variable) {
            let mut path = PathBuf::from(value);
            path.push("QEMU");
            roots.push(path);
        }
    }
    // Newer Windows installs put the firmware directly in share/; older
    // layouts kept a share/qemu/ subfolder.
    roots
        .into_iter()
        .flat_map(|root| {
            [
                root.join("share"),
                root.join("share").join("qemu"),
            ]
        })
        .map(|share| (share.join(code_name), share.join(vars_name)))
        .collect()
}

#[cfg(windows)]
fn windows_qemu_tool(name: &str) -> Option<PathBuf> {
    which(name).or_else(|| {
        ["ProgramFiles", "ProgramFiles(x86)"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(|dir| PathBuf::from(dir).join("qemu").join(format!("{name}.exe")))
            .find(|path| path.is_file())
    })
}

/// The distribution's QEMU on a Linux or Windows development host (macOS
/// always uses the built-in engine, see `qemu_engine`).
pub fn distro_qemu() -> Option<crate::qemu_engine::Engine> {
    #[cfg(windows)]
    let (qemu, qemu_img) = (windows_qemu_tool(qemu_system())?, windows_qemu_tool("qemu-img")?);
    #[cfg(not(windows))]
    let (qemu, qemu_img) = (which(qemu_system())?, which("qemu-img")?);
    let (code, vars) = distro_firmware_pairs()
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
    } else if cfg!(windows) {
        // The seed ISO is built in-process and the password hash in Rust, so
        // Windows needs only the SSH tools (curl is in System32).
        &["ssh", "ssh-keygen"]
    } else {
        &["ssh", "ssh-keygen", "xorriso", "openssl"]
    }
}

/// Build a CIDATA ISO from the staged files. macOS uses `hdiutil`, Linux
/// `xorriso`; Windows writes the image in-process (`crate::iso`).
pub fn build_seed_iso(stage: &Path, output: &Path, files: &[&str]) -> Result<()> {
    if cfg!(windows) {
        let staged: Vec<(&str, Vec<u8>)> = files
            .iter()
            .map(|name| {
                let data = std::fs::read(stage.join(name))?;
                Ok((*name, data))
            })
            .collect::<std::io::Result<_>>()?;
        let staged: Vec<(&str, &[u8])> =
            staged.iter().map(|(name, data)| (*name, data.as_slice())).collect();
        return crate::iso::build_iso(output, &staged);
    }
    let argv = seed_iso_argv(stage, output, files);
    let built = capture(&argv, Duration::from_secs(120), None);
    if built.code != 0 {
        return message(format!(
            "could not build the seed ISO: {}",
            built.output.trim()
        ));
    }
    Ok(())
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
#[cfg(not(target_os = "macos"))]
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

    #[cfg(windows)]
    #[test]
    fn seed_iso_builds_in_process_on_windows() {
        let dir = std::env::temp_dir().join(format!(
            "seed-iso-win-{}-{}",
            std::process::id(),
            crate::util::now_millis()
        ));
        let stage = dir.join("stage");
        std::fs::create_dir_all(&stage).unwrap();
        for name in ["user-data", "meta-data", "network-config"] {
            std::fs::write(stage.join(name), format!("{name}\n")).unwrap();
        }
        let iso = dir.join("seed.iso");
        build_seed_iso(&stage, &iso, &["user-data", "meta-data", "network-config"]).unwrap();
        assert!(iso.is_file());
        let bytes = std::fs::read(&iso).unwrap();
        // Volume label from the primary volume descriptor.
        let pvd = &bytes[16 * 2048..16 * 2048 + 72];
        assert_eq!(&pvd[1..6], b"CD001");
        assert_eq!(String::from_utf8_lossy(&pvd[40..46]), "CIDATA");
        let _ = std::fs::remove_dir_all(dir);
    }
}
