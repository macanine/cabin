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
    fn names_match_each_architecture() {
        let (a, x) = (GuestArch::Arm64, GuestArch::Amd64);
        assert_eq!((a.qemu_name(), x.qemu_name()), ("aarch64", "x86_64"));
        assert_eq!((a.debian(), x.debian()), ("arm64", "amd64"));
        assert_eq!(a.rust_musl_triple(), "aarch64-unknown-linux-musl");
        assert_eq!(x.rust_musl_triple(), "x86_64-unknown-linux-musl");
        assert_eq!(a.qemu_system_binary(), "qemu-system-aarch64");
        assert_eq!(x.qemu_system_binary(), "qemu-system-x86_64");
        assert_eq!(a.firmware_names(), ("edk2-aarch64-code.fd", "edk2-arm-vars.fd"));
        assert_eq!(x.firmware_names(), ("edk2-x86_64-code.fd", "edk2-i386-vars.fd"));
        assert_eq!(a.machine_type(), "virt,highmem=on");
        assert_eq!(x.machine_type(), "q35");
        assert!(a.chrome_deb_url().ends_with("_arm64.deb"));
        assert!(x.chrome_deb_url().ends_with("_amd64.deb"));
        assert_eq!(a.ua_platform(), "Linux aarch64");
        assert_eq!(x.ua_platform(), "Linux x86_64");
        assert_eq!(a.cloud_image_suffix(), "arm64");
        assert_eq!(x.cloud_image_suffix(), "amd64");
        assert!(a.device_profile_supported());
        assert!(!x.device_profile_supported());
    }

    #[test]
    fn mirrors_follow_the_archive_layout() {
        assert!(GuestArch::Arm64.apt_mirrors()[0].contains("ports.ubuntu.com"));
        assert!(GuestArch::Arm64.apt_mirrors().iter().all(|m| m.ends_with("ubuntu-ports")));
        assert!(GuestArch::Amd64.apt_mirrors()[0].contains("archive.ubuntu.com"));
        assert!(GuestArch::Amd64.apt_mirrors().iter().all(|m| m.ends_with("/ubuntu")));
    }

    #[test]
    fn engine_files_cover_binaries_and_firmware() {
        let files = GuestArch::Amd64.engine_files();
        assert!(files.iter().any(|f| f.contains("qemu-system-x86_64")));
        assert!(files.contains(&"share/qemu/edk2-i386-vars.fd".to_string()));
        let files = GuestArch::Arm64.engine_files();
        assert!(files.contains(&"share/qemu/edk2-aarch64-vars.fd".to_string()));
    }

    #[test]
    fn fingerprint_is_localized() {
        assert_eq!(
            GuestArch::Amd64.localize_fingerprint("X11; Linux aarch64)"),
            "X11; Linux x86_64)"
        );
        assert_eq!(GuestArch::Arm64.localize_fingerprint("Linux aarch64"), "Linux aarch64");
    }

    #[test]
    fn current_follows_the_host() {
        if cfg!(target_os = "macos") {
            assert_eq!(GuestArch::current(), GuestArch::Arm64);
        }
        assert!(GuestArch::host_supported());
    }
}
