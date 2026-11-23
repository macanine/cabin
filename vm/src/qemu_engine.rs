//! The QEMU the app runs, resolved in order:
//!
//! 1. the engine bundled with the app, described by `vendor`'s manifest;
//! 2. the engine an older install downloaded to `<home>/engines/qemu-<sha12>`
//!    (macOS only);
//! 3. the distribution's QEMU (Linux development hosts only).
//!
//! Windows never searches PATH or Program Files, and macOS never uses PATH or
//! Homebrew.
//!
//! The macOS build is one pinned build of the macanine/qemu fork (HVF,
//! Cocoa, SPICE/QXL, virglrenderer, its own dylibs and EDK2 firmware). In
//! development without a vendor directory it is a release asset of the fork, downloaded with the same
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
use crate::arch::GuestArch;
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
    /// A tree laid out like the macanine archive (`bin/`, `share/qemu/`).
    fn in_tree(root: &Path, arch: GuestArch) -> Self {
        let share = root.join("share").join("qemu");
        let (code, vars) = arch.firmware_names();
        // The macanine archive names its arm64 variables template differently.
        let vars = if arch == GuestArch::Arm64 {
            "edk2-aarch64-vars.fd"
        } else {
            vars
        };
        let exe = if cfg!(windows) { ".exe" } else { "" };
        Self {
            qemu: root.join("bin").join(arch.qemu_system_file()),
            qemu_img: root.join("bin").join(format!("qemu-img{exe}")),
            firmware_code: share.join(code),
            firmware_vars: share.join(vars),
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
            && self.firmware_dir.is_dir()
    }
}

impl From<crate::vendor::Vendor> for Engine {
    fn from(vendor: crate::vendor::Vendor) -> Self {
        Self {
            qemu: vendor.qemu_system(),
            qemu_img: vendor.qemu_img(),
            firmware_code: vendor.firmware_code(),
            firmware_vars: vendor.firmware_vars(),
            firmware_dir: vendor.qemu_firmware_dir(),
        }
    }
}

/// The message when the bundled engine cannot be used.
pub const BUNDLED_MISSING: &str = "Cabin's bundled QEMU is missing or damaged. Reinstall Cabin.";

/// The engine this host runs, if it is installed.
pub fn installed(home: &Path) -> Option<Engine> {
    if let Some(vendor) = crate::vendor::bundled_manifest() {
        let engine = Engine::from(vendor);
        if engine.complete() {
            return Some(engine);
        }
    }
    if cfg!(target_os = "macos") {
        let engine = Engine::in_tree(&root(home), GuestArch::current());
        return engine.complete().then_some(engine);
    }
    if cfg!(windows) {
        return None;
    }
    crate::platform::distro_qemu()
}

/// Whether a missing engine means a damaged install rather than a first run
/// that downloads it (macOS development builds without a vendor directory).
fn expects_bundle() -> bool {
    cfg!(windows)
        || (cfg!(target_os = "macos")
            && (!cfg!(debug_assertions) || crate::vendor::vendor_dir().is_some()))
}

pub(crate) fn can_download_on_first_run() -> bool {
    cfg!(target_os = "macos") && !expects_bundle()
}

/// The engine, or an error that says how to get it.
pub fn require(home: &Path) -> Result<Engine> {
    installed(home).ok_or_else(|| {
        crate::Error::Message(if expects_bundle() {
            BUNDLED_MISSING.to_string()
        } else if cfg!(target_os = "macos") {
            "The built-in QEMU is not installed yet; run the installation (step qemu) again"
                .to_string()
        } else {
            GuestArch::current().linux_install_hint().to_string()
        })
    })
}

/// `QEMU emulator version …`, or why the binary does not run.
pub fn version(engine: &Engine) -> Result<String> {
    tool_version(&engine.qemu, Duration::from_secs(60))
}

pub(crate) fn tool_version(binary: &Path, timeout: Duration) -> Result<String> {
    let result = capture(
        &[binary.display().to_string(), "--version".to_string()],
        timeout,
        None,
    );
    if result.code != 0 {
        return message(format!(
            "{} does not run (exit {}): {}",
            binary.display(),
            result.code,
            result.output.trim()
        ));
    }
    let version = result.output.lines().next().unwrap_or("").trim();
    if version.is_empty() {
        return message(format!(
            "{} does not run: no version output",
            binary.display()
        ));
    }
    Ok(version.to_string())
}

/// Download (or reuse) the pinned archive and unpack it. Returns the engine
/// and the files this created, for rollback.
pub fn install(home: &Path, log: &Arc<Mutex<File>>) -> Result<(Engine, Vec<PathBuf>)> {
    if let Some(engine) = installed(home) {
        return Ok((engine, Vec::new()));
    }
    if !can_download_on_first_run() {
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
    if !Engine::in_tree(&tree, GuestArch::current()).complete() {
        let _ = remove_path(&staging);
        return message(
            "the QEMU archive is missing qemu-system-aarch64, qemu-img or its EDK2 firmware",
        );
    }
    let _ = remove_path(&target);
    std::fs::rename(&tree, &target)?;
    let _ = remove_path(&staging);
    created.push(target.clone());
    let engine = Engine::in_tree(&target, GuestArch::current());
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
        let engine = Engine::in_tree(&dir, GuestArch::Arm64);
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

    #[test]
    fn the_vendored_engine_comes_first() {
        let dir = std::env::temp_dir().join(format!("engine-vendor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::vendor::tests::write_bundle(&dir, false);
        let vendor = crate::vendor::Vendor::load(&dir).unwrap();
        let engine: Engine = vendor.into();
        assert_eq!(engine.qemu, dir.join("qemu/bin/qemu-system-aarch64"));
        assert_eq!(engine.firmware_dir, dir.join("qemu/share/qemu"));
        assert!(engine.complete());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_version_probe_rejects_an_existing_non_executable_tool() {
        let dir = std::env::temp_dir().join(format!("engine-launch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join(GuestArch::current().qemu_system_file());
        std::fs::write(&binary, b"broken executable").unwrap();
        let error = tool_version(&binary, Duration::from_secs(1)).unwrap_err();
        assert!(error.to_string().contains("does not run"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
