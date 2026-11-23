//! AF_UNIX sockets on every host.
//!
//! QEMU's control channels (QMP, guest agent, serial console) and the gateway
//! `-netdev stream` are Unix domain sockets at the instance paths in
//! `paths.rs`. QEMU 7.1 added AF_UNIX on Windows, so the same paths work
//! there; `uds_windows` mirrors the `std::os::unix::net` API for the host side.

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use socket2::{Domain, SockAddr, Socket, Type};

/// QEMU and the host both use UTF-8 bytes, with one byte for the trailing NUL.
pub fn validate_path(path: &Path) -> crate::Result<()> {
    let limit = if cfg!(target_os = "macos") { 103 } else { 107 };
    let Some(name) = path.to_str().filter(|name| !name.contains('\0')) else {
        return crate::util::message(format!(
            "VM socket path must be valid UTF-8: {}",
            path.display()
        ));
    };
    if name.len() > limit {
        return crate::util::message(format!(
            "VM socket path is too long ({} bytes, maximum {limit}): {}. Use a shorter Cabin data directory (CABIN_HOME).",
            name.len(),
            path.display()
        ));
    }
    Ok(())
}

pub(crate) fn connect(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let addr = SockAddr::unix(path)?;
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    socket.connect_timeout(&addr, timeout)?;
    Ok(into_unix_stream(socket))
}

#[cfg(unix)]
fn into_unix_stream(socket: Socket) -> UnixStream {
    UnixStream::from(std::os::fd::OwnedFd::from(socket))
}

#[cfg(windows)]
fn into_unix_stream(socket: Socket) -> UnixStream {
    use std::os::windows::io::{FromRawSocket, IntoRawSocket};
    // SAFETY: connect created a connected AF_UNIX stream with a unique handle.
    unsafe { UnixStream::from_raw_socket(socket.into_raw_socket()) }
}

#[cfg(unix)]
fn is_socket(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    metadata.file_type().is_socket()
}

#[cfg(windows)]
fn is_socket(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // Winsock uses a reparse point for AF_UNIX. Exclude directory junctions
    // and symlinks. The connection probe also has to report a stale endpoint.
    const REPARSE_POINT: u32 = 0x400;
    metadata.is_file()
        && metadata.file_attributes() & REPARSE_POINT != 0
        && !metadata.file_type().is_symlink()
}

/// Remove only an abandoned endpoint. A live listener or unexpected file at
/// this path must be left alone, even when an old pidfile says nobody owns it.
pub(crate) fn prepare_listener(path: &Path) -> crate::Result<()> {
    validate_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_socket(&metadata) => {}
        Ok(_) => {
            return crate::util::message(format!(
                "Unexpected file at VM socket path {}; move it before starting Cabin",
                path.display()
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    match connect(path, Duration::from_millis(250)) {
        Ok(_) => crate::util::message(format!(
            "A process is already listening at {}",
            path.display()
        )),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            match fs::remove_file(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
                _ => Ok(()),
            }
        }
        Err(error) => crate::util::message(format!(
            "Could not determine whether the VM socket {} is stale: {error}",
            path.display()
        )),
    }
}

#[cfg(test)]
pub(crate) fn test_dir() -> std::path::PathBuf {
    let mut random = [0u8; 8];
    crate::util::random_bytes(&mut random);
    // The system's TMPDIR on macOS can already take ~60 bytes of sun_path.
    let base = if cfg!(unix) {
        std::path::PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let dir = base.join(format!("cabin-{}", crate::util::hex_lower(&random)));
    fs::create_dir(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_limit_counts_utf8_bytes() {
        let limit = if cfg!(target_os = "macos") { 103 } else { 107 };
        let inside = "é".repeat(limit / 2) + "a";
        assert_eq!(inside.len(), limit);
        validate_path(Path::new(&inside)).unwrap();
        let error = validate_path(Path::new(&(inside + "a"))).unwrap_err();
        assert!(error.to_string().contains(&format!("maximum {limit}")));
    }

    #[test]
    fn stale_socket_can_bind_again_but_live_listener_and_files_are_preserved() {
        let dir = test_dir();
        let socket = dir.join("qmp.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        assert!(prepare_listener(&socket).is_err());
        let _ = listener.accept().unwrap();
        drop(listener);
        prepare_listener(&socket).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        drop(listener);
        prepare_listener(&socket).unwrap();
        fs::write(&socket, b"keep").unwrap();
        assert!(prepare_listener(&socket).is_err());
        assert_eq!(fs::read(&socket).unwrap(), b"keep");
        fs::remove_dir_all(dir).unwrap();
    }
}
