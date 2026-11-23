//! Moving file contents between the sides as one tar stream.
//!
//! `pack` writes the listed paths (files, folders, symlinks) with their mode
//! and mtime. `apply` reads a header line (`{"delete": [...]}`) and then such
//! a stream, writing each file to a temporary name beside its target and
//! renaming it into place, so a reader never sees half a file. Nothing is
//! ever written through a symlinked folder, so a link created on one side
//! cannot redirect writes outside the project on the other.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::safe_relative;

/// Header line of an apply stream.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyHeader {
    /// Relative paths to remove first (deepest first). A folder that still
    /// holds something is left in place.
    pub delete: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReport {
    pub written: u64,
    pub deleted: u64,
    /// `(path, error)` for each path that could not be applied.
    pub failed: Vec<(String, String)>,
}

/// Write `paths` under `root` as a tar stream. A path that vanished since it
/// was listed is skipped (the next cycle sees it gone).
pub fn pack(root: &Path, paths: &[String], out: impl Write) -> io::Result<()> {
    let mut builder = tar::Builder::new(out);
    builder.follow_symlinks(false);
    for rel in paths {
        let Some(rel_path) = safe_relative(rel) else { continue };
        let full = root.join(rel_path);
        let Ok(meta) = fs::symlink_metadata(&full) else { continue };
        let mut header = tar::Header::new_gnu();
        header.set_mode(crate::fsx::mode_of(&meta));
        header.set_mtime(mtime_secs(&meta));
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&full)?;
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            builder.append_link(&mut header, rel_path, &target)?;
        } else if meta.is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            builder.append_data(&mut header, rel_path, io::empty())?;
        } else if meta.is_file() {
            let Ok(file) = fs::File::open(&full) else { continue };
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(meta.len());
            // The size is fixed in the header; a file that grows meanwhile
            // is cut there (the next cycle sends it again).
            builder.append_data(&mut header, rel_path, file.take(meta.len()))?;
        }
    }
    builder.finish()
}

fn mtime_secs(meta: &fs::Metadata) -> u64 {
    crate::fsx::mtime_secs_of(meta)
}

/// A header line, then a tar stream (the guest's `sync apply`).
pub fn apply_stream(root: &Path, input: impl Read) -> io::Result<ApplyReport> {
    let mut input = BufReader::with_capacity(1 << 20, input);
    let mut line = String::new();
    input.read_line(&mut line)?;
    let header: ApplyHeader = serde_json::from_str(line.trim())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad apply header: {e}")))?;
    apply(root, &header.delete, input)
}

/// Remove `delete`, then unpack `tar` under `root`.
pub fn apply(root: &Path, delete: &[String], tar: impl Read) -> io::Result<ApplyReport> {
    fs::create_dir_all(root)?;
    let mut report = ApplyReport::default();
    for rel in delete {
        match remove(root, rel) {
            Ok(true) => report.deleted += 1,
            Ok(false) => {}
            Err(error) => report.failed.push((rel.clone(), error.to_string())),
        }
    }
    let mut archive = tar::Archive::new(tar);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let rel = entry.path()?.to_string_lossy().into_owned();
        match write_entry(root, &rel, &mut entry) {
            Ok(()) => report.written += 1,
            Err(error) => {
                // Drain what is left of this entry so the stream stays aligned.
                let _ = io::copy(&mut entry, &mut io::sink());
                report.failed.push((rel, error.to_string()));
            }
        }
    }
    Ok(report)
}

/// No folder on the way to `rel` may be a symlink.
fn check_parents(root: &Path, rel: &Path) -> io::Result<()> {
    let mut current = root.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(io::Error::other(format!("{} is a symlink; not writing through it", current.display())));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn remove(root: &Path, rel: &str) -> io::Result<bool> {
    let path = safe_relative(rel).ok_or_else(|| io::Error::other("unsafe path"))?;
    check_parents(root, path)?;
    let full = root.join(path);
    let meta = match fs::symlink_metadata(&full) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if meta.is_dir() {
        // Only an empty folder goes: anything still in it was not ours to remove.
        match fs::remove_dir(&full) {
            Ok(()) => Ok(true),
            Err(_) => Ok(false),
        }
    } else {
        fs::remove_file(&full).map(|()| true)
    }
}

/// Make room at `full` for a different kind of entry.
fn clear_for(full: &Path, want_dir: bool) -> io::Result<()> {
    match fs::symlink_metadata(full) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            if !want_dir {
                // Children not explicitly deleted may be ignored or local-only.
                fs::remove_dir(full)?;
            }
        }
        Ok(_) if want_dir => fs::remove_file(full)?,
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

fn write_entry<R: Read>(root: &Path, rel: &str, entry: &mut tar::Entry<R>) -> io::Result<()> {
    let path = safe_relative(rel).ok_or_else(|| io::Error::other("unsafe path in archive"))?;
    // Earlier archive entries can replace a directory with a symlink, so a
    // successful check must never be reused for another entry.
    check_parents(root, path)?;
    let full = root.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent)?;
    }
    let header = entry.header();
    let mode = header.mode().unwrap_or(0o644) & 0o7777;
    let mtime = std::time::UNIX_EPOCH
        .checked_add(std::time::Duration::from_secs(header.mtime().unwrap_or(0)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "archive mtime is out of range"))?;
    let times = fs::FileTimes::new().set_modified(mtime).set_accessed(mtime);
    match header.entry_type() {
        tar::EntryType::Directory => {
            clear_for(&full, true)?;
            fs::create_dir_all(&full)?;
            crate::fsx::set_mode(&full, mode | 0o700)?;
        }
        tar::EntryType::Symlink => {
            let target = entry.link_name()?.ok_or_else(|| io::Error::other("symlink without a target"))?.into_owned();
            clear_for(&full, false)?;
            if fs::symlink_metadata(&full).is_ok() {
                fs::remove_file(&full)?;
            }
            crate::fsx::symlink(&target, &full)?;
            fs::set_times_nofollow(&full, times)?;
        }
        tar::EntryType::Regular | tar::EntryType::Continuous => {
            // Always stage beside the destination, including for new files.
            // Exclusive creation prevents a pre-existing file or symlink from
            // being truncated, and a random name lets concurrent applies coexist.
            let (tmp, mut file) = loop {
                let tmp = full.with_file_name(format!(".vasync-{}.tmp", uuid::Uuid::new_v4()));
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                match options.open(&tmp) {
                    Ok(file) => break (tmp, file),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            };
            let written = (|| {
                let expected = entry.size();
                if io::copy(entry, &mut file)? != expected {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "archive file ended before its declared size"));
                }
                crate::fsx::set_mode(&tmp, mode)?;
                file.set_times(times)?;
                drop(file);
                clear_for(&full, false)?;
                fs::rename(&tmp, &full)
            })();
            if written.is_err() {
                let _ = fs::remove_file(&tmp);
            }
            written?;
        }
        other => return Err(io::Error::other(format!("unsupported entry type {other:?}"))),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_tree_round_trips_with_modes_mtimes_and_symlinks() {
        use crate::sync::{SyncRules, scan};
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("bin")).unwrap();
        fs::write(src.path().join("bin/run"), "#!/bin/sh\n").unwrap();
        crate::fsx::set_mode(&src.path().join("bin/run"), 0o755).unwrap();
        fs::set_times(
            src.path().join("bin/run"),
            fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 5)),
        )
        .unwrap();
        let long = format!("{}/file.txt", "d".repeat(120));
        fs::create_dir_all(src.path().join("d".repeat(120))).unwrap();
        fs::write(src.path().join(&long), "long").unwrap();
        std::os::unix::fs::symlink("bin/run", src.path().join("link")).unwrap();

        let entries = scan(src.path(), &SyncRules::default(), None).unwrap();
        let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
        let mut stream = Vec::new();
        pack(src.path(), &paths, &mut stream).unwrap();
        let report = apply(dst.path(), &[], stream.as_slice()).unwrap();
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        let copied = scan(dst.path(), &SyncRules::default(), None).unwrap();
        for (a, b) in entries.iter().zip(&copied) {
            assert!(crate::sync::plan::same(a, b), "{a:?} vs {b:?}");
        }
        assert_eq!(crate::fsx::mode_of(&fs::metadata(dst.path().join("bin/run")).unwrap()), 0o755);
        assert_eq!(fs::read_link(dst.path().join("link")).unwrap(), Path::new("bin/run"));
        assert_eq!(fs::read_to_string(dst.path().join(&long)).unwrap(), "long");
    }

    #[test]
    fn deletes_leave_non_empty_folders_and_the_header_drives_them() {
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir_all(dst.path().join("keep")).unwrap();
        fs::write(dst.path().join("keep/theirs"), "x").unwrap();
        fs::write(dst.path().join("gone"), "x").unwrap();
        let mut stream = br#"{"delete":["gone","keep","missing"]}"#.to_vec();
        stream.push(b'\n');
        pack(dst.path(), &[], &mut stream).unwrap();
        let report = apply_stream(dst.path(), stream.as_slice()).unwrap();
        assert_eq!(report.deleted, 1);
        assert!(!dst.path().join("gone").exists());
        assert!(dst.path().join("keep/theirs").exists());
    }

    #[cfg(unix)]
    #[test]
    fn never_writes_through_a_symlinked_folder_or_outside_the_root() {
        let outside = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("x")).unwrap();
        fs::write(src.path().join("x/evil"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), dst.path().join("x")).unwrap();
        let mut stream = Vec::new();
        pack(src.path(), &["x/evil".into(), "../escape".into()], &mut stream).unwrap();
        let report = apply(dst.path(), &["x/evil".into()], stream.as_slice()).unwrap();
        assert!(!outside.path().join("evil").exists());
        assert_eq!(report.written, 0);
        assert_eq!(report.failed.len(), 2, "{:?}", report.failed);
    }

    fn append_file(builder: &mut tar::Builder<Vec<u8>>, path: &str, contents: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_size(contents.len() as u64);
        builder.append_data(&mut header, path, contents).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn an_archive_cannot_replace_a_checked_parent_with_a_symlink() {
        let dst = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut directory = tar::Header::new_gnu();
        directory.set_entry_type(tar::EntryType::Directory);
        directory.set_mode(0o755);
        directory.set_mtime(0);
        directory.set_size(0);
        builder.append_data(&mut directory, "dir", io::empty()).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_mtime(0);
        header.set_size(0);
        builder.append_link(&mut header, "dir", outside.path()).unwrap();
        append_file(&mut builder, "dir/escape", b"outside");
        let stream = builder.into_inner().unwrap();

        let report = apply(dst.path(), &[], stream.as_slice()).unwrap();
        assert_eq!(report.written, 2);
        assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
        assert_eq!(report.failed[0].0, "dir/escape");
        assert!(!outside.path().join("escape").exists());
    }

    // Observe the destination while the tar payload is being read. The
    // reader can also fail mid-entry to exercise cleanup before publication.
    struct ObservedArchive<'a> {
        bytes: &'a [u8],
        pos: usize,
        destination: &'a Path,
        expected: Option<&'a [u8]>,
        fail: bool,
        observed: bool,
    }

    impl Read for ObservedArchive<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= 512 && !self.observed {
                match self.expected {
                    Some(expected) => assert_eq!(fs::read(self.destination).unwrap(), expected),
                    None => assert!(!self.destination.exists()),
                }
                self.observed = true;
            }
            if self.fail && self.pos >= 515 {
                return Err(io::Error::other("injected payload failure"));
            }
            let limit = if self.pos < 512 {
                512 - self.pos
            } else if self.fail {
                515 - self.pos
            } else {
                self.bytes.len() - self.pos
            };
            let n = buf.len().min(limit).min(self.bytes.len() - self.pos);
            buf[..n].copy_from_slice(&self.bytes[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn files_are_published_only_after_the_full_payload_and_failures_clean_up() {
        let mut builder = tar::Builder::new(Vec::new());
        append_file(&mut builder, "file", b"new contents");
        let stream = builder.into_inner().unwrap();
        for existing in [false, true] {
            for fail in [false, true] {
                let dst = tempfile::tempdir().unwrap();
                let destination = dst.path().join("file");
                if existing {
                    fs::write(&destination, b"old contents").unwrap();
                }
                let reader = ObservedArchive {
                    bytes: &stream,
                    pos: 0,
                    destination: &destination,
                    expected: existing.then_some(b"old contents"),
                    fail,
                    observed: false,
                };
                let mut archive = tar::Archive::new(reader);
                let mut entry = archive.entries().unwrap().next().unwrap().unwrap();
                let result = write_entry(dst.path(), "file", &mut entry);
                assert_eq!(result.is_err(), fail);
                drop(entry);
                assert!(archive.into_inner().observed);
                if fail && !existing {
                    assert!(!destination.exists());
                } else {
                    assert_eq!(fs::read(&destination).unwrap(), if fail { b"old contents" } else { b"new contents" });
                }
                assert_eq!(fs::read_dir(dst.path()).unwrap().count(), usize::from(existing || !fail));
            }
        }
    }

    #[test]
    fn truncated_tar_payload_preserves_the_existing_file() {
        let dst = tempfile::tempdir().unwrap();
        let path = dst.path().join("file");
        fs::write(&path, b"original contents").unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        append_file(&mut builder, "file", b"replacement contents");
        let stream = builder.into_inner().unwrap();

        let result = apply(dst.path(), &[], &stream[..515]);
        if let Ok(report) = result {
            assert_eq!(report.written, 0);
            assert_eq!(report.failed[0].0, "file");
        }
        assert_eq!(fs::read(&path).unwrap(), b"original contents");
        assert_eq!(fs::read_dir(dst.path()).unwrap().count(), 1);
    }

    #[test]
    fn an_out_of_range_mtime_fails_the_entry_without_aborting_the_archive() {
        let dst = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_mtime(u64::MAX);
        header.set_size(1);
        builder.append_data(&mut header, "bad-time", &b"x"[..]).unwrap();
        append_file(&mut builder, "next", b"ok");
        let stream = builder.into_inner().unwrap();

        let report = apply(dst.path(), &[], stream.as_slice()).unwrap();
        assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
        assert_eq!(report.failed[0].0, "bad-time");
        assert_eq!(report.written, 1);
        assert_eq!(fs::read(dst.path().join("next")).unwrap(), b"ok");
        assert!(!dst.path().join("bad-time").exists());
    }

    #[cfg(unix)]
    #[test]
    fn existing_predictable_temp_symlink_is_never_followed_or_removed() {
        let dst = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim");
        fs::write(&victim, b"untouched").unwrap();
        fs::write(dst.path().join("file"), b"old").unwrap();
        let old_tmp = dst.path().join(format!(".file.{}.vasync-tmp", std::process::id()));
        std::os::unix::fs::symlink(&victim, &old_tmp).unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        append_file(&mut builder, "file", b"new");
        let stream = builder.into_inner().unwrap();

        let report = apply(dst.path(), &[], stream.as_slice()).unwrap();
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(fs::read(&victim).unwrap(), b"untouched");
        assert_eq!(fs::read(dst.path().join("file")).unwrap(), b"new");
        assert_eq!(fs::read_link(old_tmp).unwrap(), victim);
    }

    #[test]
    fn a_file_replaces_a_folder_after_explicit_child_deletion() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::write(src.path().join("p"), "file now").unwrap();
        fs::create_dir_all(dst.path().join("p/old")).unwrap();
        let mut stream = Vec::new();
        pack(src.path(), &["p".into()], &mut stream).unwrap();
        let report = apply(dst.path(), &["p/old".into()], stream.as_slice()).unwrap();
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(report.deleted, 1);
        assert_eq!(fs::read_to_string(dst.path().join("p")).unwrap(), "file now");
    }

    #[test]
    fn replacing_a_nonempty_folder_preserves_unlisted_contents() {
        for symlink in [false, true] {
            let dst = tempfile::tempdir().unwrap();
            fs::create_dir_all(dst.path().join("p")).unwrap();
            fs::write(dst.path().join("p/unlisted"), b"keep").unwrap();
            let mut builder = tar::Builder::new(Vec::new());
            if symlink {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_mode(0o777);
                header.set_mtime(0);
                header.set_size(0);
                builder.append_link(&mut header, "p", "target").unwrap();
            } else {
                append_file(&mut builder, "p", b"replacement");
            }
            let stream = builder.into_inner().unwrap();
            let report = apply(dst.path(), &[], stream.as_slice()).unwrap();
            assert_eq!(report.written, 0);
            assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
            assert_eq!(fs::read(dst.path().join("p/unlisted")).unwrap(), b"keep");
            assert_eq!(fs::read_dir(dst.path()).unwrap().count(), 1);
        }
    }
}
