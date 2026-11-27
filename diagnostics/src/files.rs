use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

// These open flags are OS ABI values. Keep the platform code here so the
// diagnostic crate can use them without changing its callers' dependency locks.
#[cfg(target_os = "macos")]
const UNIX_OPEN_FLAGS: i32 = 0x0100 | 0x0004; // O_NOFOLLOW | O_NONBLOCK
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const UNIX_OPEN_FLAGS: i32 = 0x8000 | 0x0800; // O_NOFOLLOW | O_NONBLOCK
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const UNIX_OPEN_FLAGS: i32 = 0x20000 | 0x0800; // O_NOFOLLOW | O_NONBLOCK
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

/// The calling application owns the containing directory. Check the actual
/// handle before reading, modifying, or locking its final path component.
pub(super) fn open_regular(path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
        #[cfg(any(
            target_os = "macos",
            all(
                target_os = "linux",
                any(target_arch = "aarch64", target_arch = "x86_64")
            ),
        ))]
        options.custom_flags(UNIX_OPEN_FLAGS);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    // Preserve the existing check on targets outside Cabin's host and VM support.
    #[cfg(not(any(
        windows,
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
    )))]
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => return Err(invalid_file()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid_file());
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::MetadataExt;

        unsafe extern "C" {
            fn geteuid() -> u32;
        }
        // SAFETY: geteuid has no preconditions and uid_t is u32 on both platforms.
        if metadata.uid() != unsafe { geteuid() } || metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "diagnostic file must be owned by this user and have one link",
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid_file());
        }
    }
    Ok(file)
}

fn invalid_file() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "expected a regular diagnostic file",
    )
}
