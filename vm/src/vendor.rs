//! The tools Cabin ships inside its app bundle: QEMU, its firmware and (on
//! Windows) OpenSSH. `scripts/fetch-vendor.mjs` writes `manifest.json` next to
//! them; every path in it is relative to the vendor directory.
//!
//! The app sets the directory at startup (`set_vendor_dir`): the bundle's
//! `vendor/` resource folder, or `build/vendor/<platform>/` in development.
//! `CABIN_VENDOR_DIR` overrides both for vmctl and tests.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

static VENDOR_DIR: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QemuEntry {
    pub version: String,
    pub system: String,
    pub img: String,
    pub firmware_code: String,
    pub firmware_vars: String,
    pub firmware_dir: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SshEntry {
    pub version: String,
    pub ssh: String,
    pub ssh_keygen: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub platform: String,
    pub guest_arch: String,
    pub qemu: QemuEntry,
    #[serde(default)]
    pub ssh: Option<SshEntry>,
}

/// Absolute paths of the vendored tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vendor {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl Vendor {
    /// Read and validate `<dir>/manifest.json`: every listed file must exist.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let manifest: Manifest = serde_json::from_str(&text)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?;
        let vendor = Self {
            dir: dir.to_path_buf(),
            manifest,
        };
        for file in vendor.files() {
            if !file.is_file() {
                return Err(format!("vendored file missing: {}", file.display()));
            }
        }
        if !vendor.qemu_firmware_dir().is_dir() {
            return Err(format!(
                "vendored folder missing: {}",
                vendor.qemu_firmware_dir().display()
            ));
        }
        Ok(vendor)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }

    fn files(&self) -> Vec<PathBuf> {
        let q = &self.manifest.qemu;
        let mut files = vec![
            self.path(&q.system),
            self.path(&q.img),
            self.path(&q.firmware_code),
            self.path(&q.firmware_vars),
        ];
        if let Some(ssh) = &self.manifest.ssh {
            files.push(self.path(&ssh.ssh));
            files.push(self.path(&ssh.ssh_keygen));
        }
        files
    }

    pub fn qemu_system(&self) -> PathBuf {
        self.path(&self.manifest.qemu.system)
    }
    pub fn qemu_img(&self) -> PathBuf {
        self.path(&self.manifest.qemu.img)
    }
    pub fn firmware_code(&self) -> PathBuf {
        self.path(&self.manifest.qemu.firmware_code)
    }
    pub fn firmware_vars(&self) -> PathBuf {
        self.path(&self.manifest.qemu.firmware_vars)
    }
    pub fn qemu_firmware_dir(&self) -> PathBuf {
        self.path(&self.manifest.qemu.firmware_dir)
    }

    /// The vendored `ssh` or `ssh-keygen`, when the bundle carries OpenSSH.
    pub fn ssh_tool(&self, name: &str) -> Option<PathBuf> {
        let ssh = self.manifest.ssh.as_ref()?;
        match name {
            "ssh" => Some(self.path(&ssh.ssh)),
            "ssh-keygen" => Some(self.path(&ssh.ssh_keygen)),
            _ => None,
        }
    }
}

/// Set once at startup by the app. A later call is ignored.
pub fn set_vendor_dir(dir: PathBuf) {
    let _ = VENDOR_DIR.set(dir);
}

/// The configured vendor directory: `CABIN_VENDOR_DIR`, else the one the app set.
pub fn vendor_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CABIN_VENDOR_DIR") {
        return Some(PathBuf::from(dir));
    }
    VENDOR_DIR.get().cloned()
}

/// The validated vendor bundle, if one is configured and complete.
pub fn vendor() -> Option<Vendor> {
    Vendor::load(&vendor_dir()?).ok()
}

/// Why the bundle is unusable (for messages), when a directory is configured.
pub fn vendor_problem() -> Option<String> {
    match vendor_dir() {
        Some(dir) => Vendor::load(&dir).err(),
        None => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn write_bundle(dir: &Path, with_ssh: bool) {
        let files = [
            "qemu/bin/qemu-system-aarch64",
            "qemu/bin/qemu-img",
            "qemu/share/qemu/edk2-aarch64-code.fd",
            "qemu/share/qemu/edk2-aarch64-vars.fd",
            "openssh/ssh.exe",
            "openssh/ssh-keygen.exe",
        ];
        for file in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"x").unwrap();
        }
        let ssh = if with_ssh {
            r#", "ssh": {"version": "9", "ssh": "openssh/ssh.exe", "sshKeygen": "openssh/ssh-keygen.exe"}"#
        } else {
            ""
        };
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"platform":"macos-arm64","guestArch":"aarch64","qemu":{{"version":"11","system":"qemu/bin/qemu-system-aarch64","img":"qemu/bin/qemu-img","firmwareCode":"qemu/share/qemu/edk2-aarch64-code.fd","firmwareVars":"qemu/share/qemu/edk2-aarch64-vars.fd","firmwareDir":"qemu/share/qemu"}}{ssh}}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn loads_and_validates_a_manifest() {
        let dir = std::env::temp_dir().join(format!("vendor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_bundle(&dir, true);
        let vendor = Vendor::load(&dir).unwrap();
        assert_eq!(vendor.qemu_system(), dir.join("qemu/bin/qemu-system-aarch64"));
        assert_eq!(
            vendor.ssh_tool("ssh-keygen"),
            Some(dir.join("openssh/ssh-keygen.exe"))
        );
        assert_eq!(vendor.ssh_tool("scp"), None);
        std::fs::remove_file(dir.join("qemu/bin/qemu-img")).unwrap();
        assert!(Vendor::load(&dir).unwrap_err().contains("qemu-img"));
        write_bundle(&dir, false);
        assert_eq!(Vendor::load(&dir).unwrap().ssh_tool("ssh"), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
