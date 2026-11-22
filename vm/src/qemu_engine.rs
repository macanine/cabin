//! The QEMU the app runs: one pinned build of the macanine/qemu fork (HVF,
//! Cocoa, SPICE/QXL, virglrenderer, its own dylibs and EDK2 firmware). It is
//! never taken from PATH or Homebrew.
//!
//! The build is a release asset of the fork, downloaded with the same
//! resumable, checksum-verified engine as the OS image (`download`), then
//! unpacked into `<home>/engines/qemu-<sha12>`. Unpacking goes to a temporary
//! folder that is renamed into place, so a present engine folder is always
//! complete. Bumping the pin installs the new build next to the old one.
//!
//! On a Linux development host (no macOS build exists) the distribution's
//! QEMU and AAVMF firmware are used instead; see `platform`.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::Result;
use crate::download::{Checksum, download_verified};
use crate::paths::StatePaths;
use crate::util::{capture, ensure_dir, log_line, message, remove_path};

/// The fork's release tag; its workflow attaches the tarball on
/// `macos-qemu-*` tags.
pub const RELEASE: &str = "macos-qemu-11.1.90-r10";
pub const ARCHIVE: &str = "qemu-macos-arm64-virgl-spice.tar.gz";
pub const SHA256: &str = "152a966dfe59b4e9518183c53f89f61e06da3947cfdf3088c5852de8aee6e5ab";

pub fn url() -> String {
    format!("https://github.com/macanine/qemu/releases/download/{RELEASE}/{ARCHIVE}")
}

pub fn checksum() -> Checksum {
    Checksum::Sha256(SHA256.to_string())
}

/// Where the pinned build's archive is downloaded.
pub fn archive_path(home: &Path) -> PathBuf {
    engines(home).join(format!("{}-{ARCHIVE}", &SHA256[..12]))
}

/// The unpacked build (`bin/`, `lib/`, `share/qemu/`).
pub fn root(home: &Path) -> PathBuf {
    engines(home).join(format!("qemu-{}", &SHA256[..12]))
}

pub(crate) fn staging_path(home: &Path) -> PathBuf {
    engines(home).join(format!(".qemu-{}.unpack", &SHA256[..12]))
}

fn engines(home: &Path) -> PathBuf {
    StatePaths::new(home).engines()
}

/// Binaries and firmware of a usable QEMU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Engine {
    pub qemu: PathBuf,
    pub qemu_img: PathBuf,
    pub firmware_code: PathBuf,
    pub firmware_vars: PathBuf,
    pub firmware_dir: PathBuf,
}

impl Engine {
    fn in_tree(root: &Path) -> Self {
        let share = root.join("share").join("qemu");
        Self {
            qemu: root.join("bin").join(if cfg!(windows) {
                "qemu-system-aarch64.exe"
            } else {
                "qemu-system-aarch64"
            }),
            qemu_img: root.join("bin").join(if cfg!(windows) {
                "qemu-img.exe"
            } else {
                "qemu-img"
            }),
            firmware_code: share.join("edk2-aarch64-code.fd"),
            firmware_vars: share.join("edk2-aarch64-vars.fd"),
            firmware_dir: share,
        }
    }

    fn complete(&self) -> bool {
        [
            &self.qemu,
            &self.qemu_img,
            &self.firmware_code,
            &self.firmware_vars,
        ]
        .iter()
        .all(|path| path.is_file())
    }
}

/// The engine this host runs, if it is installed.
pub fn installed(home: &Path) -> Option<Engine> {
    if !cfg!(target_os = "macos") {
        return crate::platform::distro_qemu();
    }
    let engine = Engine::in_tree(&root(home));
    engine.complete().then_some(engine)
}

/// The engine, or an error that says how to get it.
pub fn require(home: &Path) -> Result<Engine> {
    installed(home).ok_or_else(|| {
        crate::Error::Message(if cfg!(target_os = "macos") {
            "The built-in QEMU is not installed yet; run the installation (step qemu) again"
                .to_string()
        } else if cfg!(windows) {
            format!(
                "Install QEMU for Windows with {}, qemu-img and EDK2 firmware. Add QEMU to PATH if installed outside Program Files.",
                crate::platform::qemu_system()
            )
        } else if crate::platform::guest_arch() == "x86_64" {
            "Install qemu-system-x86, qemu-utils and ovmf on this Linux host".to_string()
        } else {
            "Install qemu-system-arm, qemu-utils and qemu-efi-aarch64 on this Linux host".to_string()
        })
    })
}

/// `QEMU emulator version …`, or why the binary does not run.
pub fn version(engine: &Engine) -> Result<String> {
    let result = capture(
        &[engine.qemu.display().to_string(), "--version".to_string()],
        Duration::from_secs(60),
        None,
    );
    if result.code != 0 {
        return message(format!(
            "{} does not run: {}",
            engine.qemu.display(),
            result.output.trim()
        ));
    }
    Ok(result
        .output
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string())
}

/// Download (or reuse) the pinned archive and unpack it. Returns the engine
/// and the files this created, for rollback.
pub fn install(home: &Path, log: &Arc<Mutex<File>>) -> Result<(Engine, Vec<PathBuf>)> {
    if !cfg!(target_os = "macos") {
        return Ok((require(home)?, Vec::new()));
    }
    let target = root(home);
    if let Some(engine) = installed(home) {
        log_line(
            log,
            &format!("built-in QEMU already unpacked: {}", target.display()),
        );
        return Ok((engine, Vec::new()));
    }
    let archive = archive_path(home);
    let mut created = Vec::new();
    if !archive.exists() {
        created.push(archive.clone());
    }
    download_verified(&url(), &archive, &checksum(), log)?;

    let staging = staging_path(home);
    let _ = remove_path(&staging);
    ensure_dir(&staging)?;
    log_line(log, &format!("unpacking {}", archive.display()));
    let unpacked = capture(
        &[
            "tar".to_string(),
            "-xzf".to_string(),
            archive.display().to_string(),
            "-C".to_string(),
            staging.display().to_string(),
        ],
        Duration::from_secs(300),
        None,
    );
    if unpacked.code != 0 {
        let _ = remove_path(&staging);
        return message(format!(
            "could not unpack the QEMU archive: {}",
            unpacked.output.trim()
        ));
    }
    // The archive holds a single top-level `qemu/` folder.
    let tree = staging.join("qemu");
    if !Engine::in_tree(&tree).complete() {
        let _ = remove_path(&staging);
        return message(
            "the QEMU archive is missing qemu-system-aarch64, qemu-img or its EDK2 firmware",
        );
    }
    let _ = remove_path(&target);
    std::fs::rename(&tree, &target)?;
    let _ = remove_path(&staging);
    created.push(target.clone());
    let engine = Engine::in_tree(&target);
    log_line(
        log,
        &format!("built-in QEMU unpacked: {}", target.display()),
    );
    Ok((engine, created))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pin_is_a_release_asset_with_a_valid_digest() {
        assert!(checksum().is_well_formed());
        assert!(
            url().starts_with("https://github.com/macanine/qemu/releases/download/macos-qemu-")
        );
        assert!(url().ends_with(ARCHIVE));
        let home = Path::new("/tmp/virt-home");
        assert!(root(home).starts_with(engines(home)));
        assert_ne!(root(home), archive_path(home));
    }

    #[test]
    fn an_engine_needs_both_binaries_and_aarch64_firmware() {
        let dir = std::env::temp_dir().join(format!("qemu-engine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let engine = Engine::in_tree(&dir);
        assert!(!engine.complete());
        for path in [&engine.qemu, &engine.qemu_img, &engine.firmware_code] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
        }
        assert!(!engine.complete(), "the variables template is required too");
        std::fs::write(&engine.firmware_vars, b"").unwrap();
        assert!(engine.complete());
        let _ = std::fs::remove_dir_all(dir);
    }
}
