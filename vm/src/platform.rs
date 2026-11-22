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
use crate::arch::GuestArch;
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

/// Whether WHPX can be tried: the feature's DLL is present, no WHPX start
/// failed in this process and no probe rejected it.
#[cfg(windows)]
pub fn whpx_available() -> bool {
    whpx_enabled()
        && crate::accel::fallback_reason().is_none()
        && crate::accel::cached_whpx_problem().is_none()
}

#[cfg(not(windows))]
pub fn whpx_available() -> bool {
    false
}

pub fn accel() -> Accel {
    if cfg!(target_os = "macos") {
        return Accel {
            accel: "hvf",
            cpu: "host",
            emulated: false,
        };
    }
    let arch = GuestArch::current();
    // `pauth-impdef` replaces QARMA pointer authentication with a cheap
    // implementation-defined algorithm: a large speed-up under TCG.
    let tcg = Accel {
        accel: "tcg",
        cpu: match arch {
            GuestArch::Arm64 => "max,pauth-impdef=on",
            GuestArch::Amd64 => "max",
        },
        emulated: true,
    };
    if cfg!(windows) {
        if arch == GuestArch::Amd64 && whpx_available() {
            return Accel {
                accel: "whpx",
                // WHPX has no host CPU passthrough model; QEMU's default is
                // the safe choice (adding features it cannot virtualize
                // fails the launch).
                cpu: "",
                emulated: false,
            };
        }
        return tcg;
    }
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
        tcg
    }
}

impl Accel {
    /// The QEMU arguments selecting this accelerator and CPU for `-machine`.
    /// WHPX uses `-accel` with the irqchip off (QEMU's in-kernel irqchip is not
    /// available there); the others use `-machine …,accel=`.
    pub fn qemu_args(&self, arch: GuestArch) -> Vec<String> {
        let mut args = Vec::new();
        if self.accel == "whpx" {
            args.extend(["-accel".to_string(), "whpx,kernel-irqchip=off".to_string()]);
            args.extend(["-machine".to_string(), arch.machine_type().to_string()]);
        } else {
            args.extend([
                "-machine".to_string(),
                format!("{},accel={}", arch.machine_type(), self.accel),
            ]);
        }
        if !self.cpu.is_empty() {
            args.extend(["-cpu".to_string(), self.cpu.to_string()]);
        }
        args
    }
}

/// Extra time factor for guest-side waits under emulation.
pub fn slowdown() -> u32 {
    if accel().emulated { 8 } else { 1 }
}

/// The QEMU binary name for this host's guest architecture.
pub fn qemu_system() -> &'static str {
    GuestArch::current().qemu_system_binary()
}

/// Linux distribution firmware pairs (code, writable variables template).
#[cfg(unix)]
pub fn distro_firmware_pairs() -> Vec<(PathBuf, PathBuf)> {
    GuestArch::current().distro_firmware_paths()
}

/// Windows firmware pairs: the qemu.org installer ships the EDK2 images next
/// to the binaries (`share\edk2-*.fd` on current installs; older layouts
/// kept a `share\qemu` subfolder).
#[cfg(windows)]
pub fn distro_firmware_pairs() -> Vec<(PathBuf, PathBuf)> {
    let (code_name, vars_name) = GuestArch::current().firmware_names();
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
