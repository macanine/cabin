//! Host-platform differences: CPU acceleration, UEFI firmware locations and
//! the few Unix-only process and permission checks.
//!
//! The product targets macOS (HVF). Linux is supported for development and
//! testing: KVM when `/dev/kvm` is usable, otherwise TCG emulation. Windows
//! uses a native-architecture guest and the bundled QEMU and OpenSSH. Every
//! host builds the seed ISO in process (`iso`).

use std::path::{Path, PathBuf};

use crate::Result;
use crate::arch::GuestArch;
use crate::util::which;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accel {
    /// Value for `-machine …,accel=…`.
    pub accel: &'static str,
    /// Value for `-cpu` (empty: QEMU's default, e.g. under WHPX).
    pub cpu: &'static str,
    /// Software emulation: much slower; timeouts should be generous.
    pub emulated: bool,
}

/// Whether the Windows Hypervisor Platform feature is enabled (WHPX needs it);
/// false on every other OS, where `SystemRoot` is unset.
fn whpx_enabled() -> bool {
    cfg!(windows)
        && std::env::var_os("SystemRoot").is_some_and(|root| whpx_library_present(Path::new(&root)))
}

fn whpx_library_present(system_root: &Path) -> bool {
    system_root
        .join("System32")
        .join("WinHvPlatform.dll")
        .is_file()
}

pub(crate) fn virtualization_disabled_in_firmware() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn IsProcessorFeaturePresent(feature: u32) -> i32;
        }
        // SAFETY: PF_VIRT_FIRMWARE_ENABLED is a Win32 processor feature ID.
        unsafe { IsProcessorFeaturePresent(21) == 0 }
    }
    #[cfg(not(windows))]
    false
}

/// Windows reports mapped network paths as drive letters.
pub(crate) fn mapped_network_drive(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDriveTypeW(root: *const u16) -> u32;
        }
        let Some(Component::Prefix(prefix)) = path.components().next() else {
            return false;
        };
        let (Prefix::Disk(letter) | Prefix::VerbatimDisk(letter)) = prefix.kind() else {
            return false;
        };
        let root = [u16::from(letter), b':' as u16, b'\\' as u16, 0];
        // SAFETY: root is a NUL-terminated drive root. DRIVE_REMOTE is 4.
        unsafe { GetDriveTypeW(root.as_ptr()) == 4 }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

/// Whether WHPX can be tried: the feature's DLL is present, no WHPX start
/// failed in this process and no probe rejected it.
pub fn whpx_available() -> bool {
    whpx_enabled()
        && crate::accel::fallback_reason().is_none()
        && crate::accel::cached_whpx_problem().is_none()
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
pub fn distro_firmware_pairs() -> Vec<(PathBuf, PathBuf)> {
    GuestArch::current().distro_firmware_paths()
}

/// The distribution's QEMU on a Linux development host. macOS uses the
/// bundled or downloaded engine and Windows only the bundled one (see
/// `qemu_engine`), so this is `None` there and never searches PATH.
pub fn distro_qemu() -> Option<crate::qemu_engine::Engine> {
    if !cfg!(target_os = "linux") {
        return None;
    }
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

/// `ssh`, `ssh-keygen` or another OpenSSH tool: the bundled one on Windows,
/// otherwise the first on PATH.
pub fn ssh_tool(name: &str) -> Option<PathBuf> {
    if cfg!(windows)
        && let Some(path) = bundled_ssh_tool(name)
    {
        return Some(path);
    }
    which(name)
}

pub(crate) fn bundled_ssh_tool(name: &str) -> Option<PathBuf> {
    crate::vendor::bundled_manifest()
        .and_then(|vendor| vendor.ssh_tool(name))
        .filter(|path| path.is_file())
}

/// `argv` with a leading `ssh` or `ssh-keygen` replaced by its resolved path.
pub fn resolve_ssh_argv(mut argv: Vec<String>) -> Vec<String> {
    if let Some(first) = argv.first_mut()
        && matches!(first.as_str(), "ssh" | "ssh-keygen")
        && let Some(path) = ssh_tool(first)
    {
        *first = path.display().to_string();
    }
    argv
}

/// Tools the VM pipeline needs on every host. The seed ISO and the password
/// hash are built in process.
pub fn required_tools() -> &'static [&'static str] {
    &["ssh", "ssh-keygen"]
}

/// Build the CIDATA seed ISO in process from the staged files.
pub fn build_seed_iso(stage: &Path, output: &Path, files: &[&str]) -> Result<()> {
    let staged: Vec<(&str, Vec<u8>)> = files
        .iter()
        .map(|name| Ok((*name, std::fs::read(stage.join(name))?)))
        .collect::<std::io::Result<_>>()?;
    let staged: Vec<(&str, &[u8])> = staged
        .iter()
        .map(|(name, data)| (*name, data.as_slice()))
        .collect();
    crate::iso::build_iso(output, &staged)
}

// ---- where the operating systems genuinely differ -------------------------

/// Native machine architecture, including when Windows runs Cabin under emulation.
pub fn native_arch() -> &'static str {
    #[cfg(windows)]
    if let Some(arch) = windows_native_arch() {
        return arch;
    }
    std::env::consts::ARCH
}

#[cfg(windows)]
fn windows_native_arch() -> Option<&'static str> {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
        fn GetCurrentProcess() -> *mut c_void;
    }
    type IsWow64Process2 = unsafe extern "system" fn(*mut c_void, *mut u16, *mut u16) -> i32;
    let kernel32: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
    // SAFETY: call the documented Win32 API with the current process handle
    // and valid out pointers. Resolve it dynamically for older Windows builds.
    unsafe {
        let module = GetModuleHandleW(kernel32.as_ptr());
        if module.is_null() {
            return None;
        }
        let function = GetProcAddress(module, c"IsWow64Process2".as_ptr().cast());
        if function.is_null() {
            return None;
        }
        let query: IsWow64Process2 = std::mem::transmute(function);
        let (mut process, mut native) = (0, 0);
        if query(GetCurrentProcess(), &mut process, &mut native) == 0 {
            return None;
        }
        match native {
            0xaa64 | 0x01c4 => Some("aarch64"),
            0x8664 => Some("x86_64"),
            0x014c => Some("x86"),
            _ => None,
        }
    }
}

/// Whether the directory is private to this user. Windows access lists
/// already scope a profile directory to its user.
pub fn owner_only(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid has no preconditions.
        metadata.permissions().mode() & 0o777 == 0o700
            && metadata.uid() == unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

/// Whether this process runs as root (never true on Windows).
pub fn is_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Start a spawned tool in its own session so a terminal hang-up cannot reach
/// it (Windows children are already independent).
pub fn detach_session(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches no shared state.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = command;
}

mod command_tree;
pub use command_tree::CommandTree;

pub mod command_pipe {
    use std::io::{self, PipeReader, PipeWriter, Read};

    #[cfg(windows)]
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    #[cfg(windows)]
    use std::ptr::{null, null_mut};
    #[cfg(windows)]
    use windows_sys::Win32::System::Pipes::{
        CreatePipe, PIPE_NOWAIT, PeekNamedPipe, SetNamedPipeHandleState,
    };

    #[cfg(unix)]
    use io::pipe;

    #[cfg(windows)]
    fn pipe() -> io::Result<(PipeReader, PipeWriter)> {
        let (mut reader, mut writer) = (null_mut(), null_mut());
        // SAFETY: These are out parameters. Null security attributes make
        // the new handles non-inheritable until passed to Command.
        if unsafe { CreatePipe(&mut reader, &mut writer, null(), 64 * 1024) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreatePipe returned two new owned handles.
        Ok(unsafe {
            (
                OwnedHandle::from_raw_handle(reader).into(),
                OwnedHandle::from_raw_handle(writer).into(),
            )
        })
    }

    pub fn input() -> io::Result<(PipeReader, PipeWriter)> {
        let (reader, writer) = pipe()?;
        nonblocking(&writer)?;
        Ok((reader, writer))
    }

    pub fn output() -> io::Result<(PipeReader, PipeWriter)> {
        let (reader, writer) = pipe()?;
        #[cfg(unix)]
        nonblocking(&reader)?;
        Ok((reader, writer))
    }

    #[cfg(unix)]
    fn nonblocking(pipe: &impl std::os::fd::AsRawFd) -> io::Result<()> {
        let fd = pipe.as_raw_fd();
        // SAFETY: pipe owns this open fd and both fcntl calls use integer flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(windows)]
    fn nonblocking(pipe: &PipeWriter) -> io::Result<()> {
        // SAFETY: Only the host's write handle changes mode. The child's read
        // handle still blocks normally.
        if unsafe { SetNamedPipeHandleState(pipe.as_raw_handle(), &PIPE_NOWAIT, null(), null()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(unix)]
    pub fn read(pipe: &mut PipeReader, buffer: &mut [u8]) -> io::Result<usize> {
        pipe.read(buffer)
    }

    #[cfg(windows)]
    pub fn read(pipe: &mut PipeReader, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut available = 0;
        // SAFETY: this is the sole read handle, with no concurrent reads. The
        // pipe stays open through the call and only `available` is written.
        let ok = unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                null_mut(),
                0,
                null_mut(),
                &mut available,
                null_mut(),
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            return if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof
            ) {
                Ok(0)
            } else {
                Err(error)
            };
        }
        if available == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let count = (available as usize).min(buffer.len());
        pipe.read(&mut buffer[..count])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whpx_uses_the_windows_hypervisor_platform_library() {
        let dir = std::env::temp_dir().join(format!("whpx-library-{}", std::process::id()));
        let system = dir.join("System32");
        std::fs::create_dir_all(&system).unwrap();
        std::fs::write(system.join("WindowsHypervisorPlatform.dll"), b"").unwrap();
        assert!(!whpx_library_present(&dir));
        std::fs::write(system.join("WinHvPlatform.dll"), b"").unwrap();
        assert!(whpx_library_present(&dir));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
