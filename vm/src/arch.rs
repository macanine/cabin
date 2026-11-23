//! The single source of architecture decisions for the Ubuntu guest.
//!
//! The macOS product always runs an arm64 guest (HVF). Other hosts run their
//! native architecture so hardware acceleration (KVM, WHPX) stays possible.
//! Nothing else in the workspace should branch on `aarch64` or `x86_64`.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GuestArch {
    Arm64,
    Amd64,
}

impl GuestArch {
    /// The guest this host runs: arm64 on macOS, the host architecture elsewhere.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") || cfg!(target_arch = "aarch64") {
            Self::Arm64
        } else {
            Self::Amd64
        }
    }

    /// Whether this host can run the guest at all (natively or emulated).
    pub const fn host_supported() -> bool {
        cfg!(any(target_arch = "aarch64", target_arch = "x86_64"))
    }

    /// QEMU's name for the architecture (`qemu-system-<name>`).
    pub const fn qemu_name(self) -> &'static str {
        match self {
            Self::Arm64 => "aarch64",
            Self::Amd64 => "x86_64",
        }
    }

    /// Debian/Ubuntu architecture name.
    pub const fn debian(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::Amd64 => "amd64",
        }
    }

    /// Short human label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Arm64 => "ARM64",
            Self::Amd64 => "x86_64",
        }
    }

    /// Rust target triple of the static guest binaries.
    pub const fn rust_musl_triple(self) -> &'static str {
        match self {
            Self::Arm64 => "aarch64-unknown-linux-musl",
            Self::Amd64 => "x86_64-unknown-linux-musl",
        }
    }

    /// `qemu-system-<arch>` without a platform suffix.
    pub const fn qemu_system_binary(self) -> &'static str {
        match self {
            Self::Arm64 => "qemu-system-aarch64",
            Self::Amd64 => "qemu-system-x86_64",
        }
    }

    /// `qemu-system-<arch>` with `.exe` on Windows.
    pub fn qemu_system_file(self) -> String {
        format!("{}{}", self.qemu_system_binary(), exe_suffix())
    }

    /// EDK2 firmware file names QEMU ships: (code, writable variables template).
    pub const fn firmware_names(self) -> (&'static str, &'static str) {
        match self {
            Self::Arm64 => ("edk2-aarch64-code.fd", "edk2-arm-vars.fd"),
            Self::Amd64 => ("edk2-x86_64-code.fd", "edk2-i386-vars.fd"),
        }
    }

    /// Firmware names inside the macanine macOS engine archive (arm64 only).
    pub const fn engine_firmware_names(self) -> (&'static str, &'static str) {
        match self {
            Self::Arm64 => ("edk2-aarch64-code.fd", "edk2-aarch64-vars.fd"),
            Self::Amd64 => ("edk2-x86_64-code.fd", "edk2-i386-vars.fd"),
        }
    }

    /// Files an engine tree must contain, relative to its root.
    pub fn engine_files(self) -> Vec<String> {
        let (code, vars) = self.engine_firmware_names();
        vec![
            format!("bin/{}", self.qemu_system_file()),
            format!("bin/qemu-img{}", exe_suffix()),
            format!("share/qemu/{code}"),
            format!("share/qemu/{vars}"),
        ]
    }

    /// `-machine` type, without the accelerator.
    pub const fn machine_type(self) -> &'static str {
        match self {
            Self::Arm64 => "virt,highmem=on",
            Self::Amd64 => "q35",
        }
    }

    pub const fn chrome_deb_url(self) -> &'static str {
        match self {
            Self::Arm64 => "https://dl.google.com/linux/direct/google-chrome-stable_current_arm64.deb",
            Self::Amd64 => "https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb",
        }
    }

    /// `navigator.platform` and the platform part of the Chrome user agent.
    pub const fn ua_platform(self) -> &'static str {
        match self {
            Self::Arm64 => "Linux aarch64",
            Self::Amd64 => "Linux x86_64",
        }
    }

    /// Ubuntu archive mirrors: the one the image uses, then others commonly
    /// faster in Asia. arm64 lives on `ubuntu-ports`.
    pub const fn apt_mirrors(self) -> &'static [&'static str] {
        match self {
            Self::Arm64 => &[
                "http://ports.ubuntu.com/ubuntu-ports",
                "https://ftp.kaist.ac.kr/ubuntu-ports",
                "https://mirrors.nju.edu.cn/ubuntu-ports",
            ],
            Self::Amd64 => &[
                "http://archive.ubuntu.com/ubuntu",
                "https://ftp.kaist.ac.kr/ubuntu",
                "https://mirrors.nju.edu.cn/ubuntu",
            ],
        }
    }

    /// Suffix of the minimal cloud image name: `<arch>.img` in
    /// `ubuntu-<version>-minimal-cloudimg-<arch>.img`.
    pub const fn cloud_image_suffix(self) -> &'static str {
        self.debian()
    }

    /// The `Accept` architecture of the CPU in the browser fingerprint.
    pub const fn cpu_architecture(self) -> &'static str {
        match self {
            Self::Arm64 => "arm",
            Self::Amd64 => "x86",
        }
    }

    /// Whether the Apple-silicon device profile can run on this guest.
    pub const fn device_profile_supported(self) -> bool {
        matches!(self, Self::Arm64)
    }

    /// Distro install hint for Linux development hosts.
    pub const fn linux_install_hint(self) -> &'static str {
        match self {
            Self::Arm64 => "Install qemu-system-arm, qemu-utils and qemu-efi-aarch64 on this Linux host",
            Self::Amd64 => "Install qemu-system-x86, qemu-utils and ovmf on this Linux host",
        }
    }

    /// Linux distribution firmware pairs (code, vars template).
    pub fn distro_firmware_paths(self) -> Vec<(PathBuf, PathBuf)> {
        let pairs: [(&str, &str); 3] = match self {
            Self::Amd64 => [
                ("/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/OVMF/OVMF_VARS.fd"),
                ("/usr/share/qemu/edk2-x86_64-code.fd", "/usr/share/qemu/edk2-i386-vars.fd"),
                ("/usr/share/OVMF/OVMF_CODE_4M.fd", "/usr/share/OVMF/OVMF_VARS_4M.fd"),
            ],
            Self::Arm64 => [
                ("/usr/share/AAVMF/AAVMF_CODE.fd", "/usr/share/AAVMF/AAVMF_VARS.fd"),
                ("/usr/share/qemu/edk2-aarch64-code.fd", "/usr/share/qemu/edk2-arm-vars.fd"),
                (
                    "/usr/share/edk2/aarch64/QEMU_EFI-pflash.raw",
                    "/usr/share/edk2/aarch64/vars-template-pflash.raw",
                ),
            ],
        };
        pairs.iter().map(|(c, v)| (PathBuf::from(c), PathBuf::from(v))).collect()
    }

    /// Substitute this guest's platform into browser fingerprint text written
    /// for arm64 (`Linux aarch64`, `architecture: arm`).
    pub fn localize_fingerprint(self, text: &str) -> String {
        text.replace("Linux aarch64", self.ua_platform())
    }
}

const fn exe_suffix() -> &'static str {
    if cfg!(windows) { ".exe" } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_architecture_maps_to_its_own_names() {
        for (arch, qemu, deb, machine, vars, profile) in [
            (GuestArch::Arm64, "aarch64", "arm64", "virt,highmem=on", "edk2-arm-vars.fd", true),
            (GuestArch::Amd64, "x86_64", "amd64", "q35", "edk2-i386-vars.fd", false),
        ] {
            assert_eq!(arch.qemu_name(), qemu);
            assert_eq!(arch.debian(), deb);
            assert_eq!(arch.machine_type(), machine);
            assert_eq!(arch.firmware_names().1, vars);
            assert_eq!(arch.device_profile_supported(), profile);
            assert!(arch.chrome_deb_url().ends_with(&format!("_{deb}.deb")));
            assert!(arch.localize_fingerprint("Linux aarch64").ends_with(&format!("Linux {qemu}")));
        }
    }
}
