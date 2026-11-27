//! Publish staged entries while retaining an existing entry on failure.

use std::fs;
use std::io;
use std::path::Path;

pub(super) fn stage(root: &Path) -> io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(super::super::STAGED_FILE_PREFIX)
        .suffix(".tmp")
        .tempdir_in(root)
}

pub(super) fn replace(source: &Path, destination: &Path, new: bool) -> io::Result<()> {
    if new {
        return rename_new(source, destination);
    }
    let old = match fs::symlink_metadata(destination) {
        Ok(old) => old,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return rename_new(source, destination);
        }
        Err(error) => return Err(error),
    };
    let staged = fs::symlink_metadata(source)?;
    if !directory_entry(&old) && !directory_entry(&staged) {
        return with_writable(destination, &old, || fs::rename(source, destination));
    }

    // Renames across entry types can require a vacant destination. Move the
    // old entry aside first, and retain it if either rollback or cleanup fails.
    if old.is_dir() && fs::read_dir(destination)?.next().transpose()?.is_some() {
        return Err(io::Error::from(io::ErrorKind::DirectoryNotEmpty));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("entry has no parent"))?;
    let holder = stage(parent)?;
    let backup = holder.path().join("original");
    rename_new(destination, &backup)?;
    if let Err(error) = rename_new(source, destination) {
        if let Err(rollback) = rename_new(&backup, destination) {
            let _ = holder.keep();
            return Err(io::Error::other(format!(
                "{error}; could not restore the original at {}: {rollback}",
                backup.display()
            )));
        }
        return Err(error);
    }
    if let Err(error) = super::remove_file_or_link(&backup, &old) {
        let _ = holder.keep();
        return Err(io::Error::other(format!(
            "replacement published; original retained at {}: {error}",
            backup.display()
        )));
    }
    Ok(())
}

fn directory_entry(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if meta.file_type().is_symlink_dir() {
            return true;
        }
    }
    meta.is_dir()
}

pub(super) fn with_writable<T>(
    path: &Path,
    meta: &fs::Metadata,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    #[cfg(windows)]
    if meta.is_file() && meta.permissions().readonly() {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const READONLY: u32 = 1;
        // Use a handle so restoring attributes can only affect the old file.
        let old = fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let original = old.metadata()?.file_attributes();
        set_attributes(&old, original & !READONLY)?;
        let result = operation();
        if let Err(error) = result {
            set_attributes(&old, original).map_err(|restore| {
                io::Error::other(format!(
                    "{error}; could not restore read-only attribute: {restore}"
                ))
            })?;
            return Err(error);
        }
        return result;
    }
    #[cfg(not(windows))]
    let _ = (path, meta);
    operation()
}

#[cfg(windows)]
fn set_attributes(file: &fs::File, attributes: u32) -> io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::io::{AsRawHandle, RawHandle};

    #[repr(C)]
    struct FileBasicInfo {
        times: [i64; 4],
        attributes: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileInformationByHandle(
            file: RawHandle,
            class: i32,
            info: *const c_void,
            len: u32,
        ) -> i32;
    }
    const FILE_BASIC_INFO: i32 = 0;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
    let info = FileBasicInfo {
        times: [0; 4],
        attributes: if attributes == 0 {
            FILE_ATTRIBUTE_NORMAL
        } else {
            attributes
        },
    };
    // SAFETY: The handle has attribute access and the struct is Win32 FILE_BASIC_INFO.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FILE_BASIC_INFO,
            (&raw const info).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn a_locked_readonly_destination_keeps_its_attributes_and_contents() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original");
        let incoming = root.path().join("incoming");
        fs::write(&original, b"old content").unwrap();
        fs::write(&incoming, b"new content").unwrap();
        let mut permissions = fs::metadata(&original).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&original, permissions).unwrap();
        // The source handle makes rename fail even on filesystems that support
        // replacing an open destination while preserving its existing handles.
        let reader = fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&incoming)
            .unwrap();
        assert!(replace(&incoming, &original, false).is_err());
        assert_eq!(fs::read(&original).unwrap(), b"old content");
        assert!(fs::metadata(&original).unwrap().permissions().readonly());
        drop(reader);
        replace(&incoming, &original, false).unwrap();
        assert_eq!(fs::read(&original).unwrap(), b"new content");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn rename_new(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    // SAFETY: Both strings are NUL-terminated and live through the syscall.
    let result = unsafe {
        #[cfg(target_os = "linux")]
        {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        }
        #[cfg(target_os = "macos")]
        {
            libc::renameatx_np(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_EXCL,
            )
        }
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
pub(super) fn rename_new(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32;
    }

    let wide = |path: &Path| -> io::Result<Vec<u16>> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("entry has no parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("entry has no name"))?;
        let full = parent.canonicalize()?.join(name);
        let mut wide: Vec<_> = full.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        wide.push(0);
        Ok(wide)
    };
    let source = wide(source)?;
    let destination = wide(destination)?;
    // SAFETY: The paths are NUL-terminated UTF-16. No replace or copy flags.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(super) fn rename_new(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}
