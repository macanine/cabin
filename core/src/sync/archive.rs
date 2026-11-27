//! Moving file contents between the sides as one tar stream.
//!
//! `pack` writes the listed paths (files, folders, symlinks) with their mode
//! and mtime. `apply` reads a header line (`{"delete": [...]}`) and then such
//! a stream, writing each file to a temporary name beside its target and
//! renaming it into place, so a reader never sees half a file. Nothing is
//! ever written through a symlinked folder, so a link created on one side
//! cannot redirect writes outside the project on the other.

mod git_copy;
mod publish;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::plan::Stamp;
use super::scan::{self, Entry, EntryKind, FileDigest};
use super::{check_parents, join_wire_path, safe_relative};

/// Header line of an apply stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ApplyHeader {
    // Old receivers require a top-level `delete` field and reject this request
    // before applying anything, even if the worker changed after the scan.
    Checked {
        checked: CheckedApplyHeader,
    },
    GitCopy {
        #[serde(rename = "gitCopy")]
        git_copy: ApplyChecks,
    },
    Legacy {
        delete: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckedApplyHeader {
    delete: Vec<String>,
    checks: ApplyChecks,
}

impl ApplyHeader {
    pub fn new(delete: Vec<String>, checks: Option<ApplyChecks>) -> Self {
        match checks {
            Some(checks) => Self::Checked {
                checked: CheckedApplyHeader { delete, checks },
            },
            None => Self::Legacy { delete },
        }
    }

    pub fn git_copy(checks: ApplyChecks) -> Self {
        Self::GitCopy { git_copy: checks }
    }
}

/// The source and destination observed by the cycle before it planned an apply.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplyChecks {
    pub writes: BTreeMap<String, WriteCheck>,
    pub deletes: BTreeMap<String, Stamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteCheck {
    pub source: Stamp,
    pub destination: Option<Stamp>,
}

impl ApplyChecks {
    pub fn for_paths(
        source: &BTreeMap<String, Entry>,
        destination: &BTreeMap<String, Entry>,
        write: &[String],
        delete: &[String],
    ) -> io::Result<Self> {
        let mut checks = Self::default();
        for path in write {
            let entry = source.get(path).ok_or_else(|| {
                io::Error::other(format!("{path}: no source entry for the planned write"))
            })?;
            checks.writes.insert(
                path.clone(),
                WriteCheck {
                    source: entry.into(),
                    destination: destination.get(path).map(Stamp::from),
                },
            );
        }
        for path in delete {
            let entry = destination.get(path).ok_or_else(|| {
                io::Error::other(format!(
                    "{path}: no destination entry for the planned deletion"
                ))
            })?;
            checks.deletes.insert(path.clone(), entry.into());
        }
        Ok(checks)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReport {
    pub written: u64,
    pub deleted: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_nonempty: Vec<String>,
    /// Deletions skipped because the source still holds the path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_present: Vec<String>,
    /// `(path, error)` for each path that could not be applied.
    pub failed: Vec<(String, String)>,
}

/// Write every requested path under `root` or fail the transfer.
pub fn pack(root: &Path, paths: &[String], out: impl Write) -> io::Result<()> {
    let mut builder = tar::Builder::new(PackOutput {
        out,
        aborted: false,
    });
    builder.follow_symlinks(false);
    for rel in paths {
        if let Err(error) = pack_entry(root, rel, &mut builder) {
            // The tar builder writes its end blocks on drop. After a short
            // read they could be consumed as file contents by the receiver.
            builder.get_mut().aborted = true;
            return Err(io::Error::new(error.kind(), format!("{rel}: {error}")));
        }
    }
    builder.finish()
}

/// A filter can hide a path that is still present on disk. Keep it out of
/// the deletion batch while allowing unrelated edits to finish.
pub fn present_deletions(root: &Path, paths: &[String]) -> io::Result<Vec<String>> {
    if !paths.is_empty() && !root.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "sync root is not a folder",
        ));
    }
    let mut present = Vec::new();
    'paths: for rel in paths {
        let path = safe_relative(rel).ok_or_else(|| io::Error::other("unsafe path"))?;
        let mut current = root.to_path_buf();
        let mut components = path.components().peekable();
        while let Some(part) = components.next() {
            current.push(part);
            match fs::symlink_metadata(&current) {
                // The scan never follows links or traverses a regular file.
                Ok(meta)
                    if components.peek().is_some()
                        && (meta.file_type().is_symlink() || !meta.is_dir()) =>
                {
                    continue 'paths;
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue 'paths,
                Err(error) => return Err(io::Error::new(error.kind(), format!("{rel}: {error}"))),
            }
        }
        present.push(rel.clone());
    }
    Ok(present)
}

struct PackOutput<W> {
    out: W,
    aborted: bool,
}

impl<W: Write> Write for PackOutput<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.aborted {
            Ok(buf.len())
        } else {
            self.out.write(buf)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

struct PackFile<'a> {
    file: io::Take<fs::File>,
    opened: fs::Metadata,
    path: &'a Path,
}

impl Read for PackFile<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.file.read(buf)?;
        if n == 0
            && !buf.is_empty()
            && (self.file.limit() != 0
                || !scan::same_file_snapshot(&self.opened, &self.file.get_ref().metadata()?)
                || !scan::same_file_snapshot(&self.opened, &fs::symlink_metadata(self.path)?))
        {
            return Err(scan::unstable_file(self.path));
        }
        Ok(n)
    }
}

fn pack_entry(root: &Path, rel: &str, builder: &mut tar::Builder<impl Write>) -> io::Result<()> {
    let rel_path = safe_relative(rel).ok_or_else(|| io::Error::other("unsafe path"))?;
    check_parents(root, rel_path)?;
    let full = join_wire_path(root, rel);
    let meta = fs::symlink_metadata(&full)?;
    let mut header = tar::Header::new_gnu();
    header.set_mode(crate::fsx::mode_of(&meta));
    header.set_mtime(crate::fsx::mtime_secs_of(&meta));
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
        let (file, opened) = scan::open_regular(&full, &meta)?;
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(opened.len());
        let reader = PackFile {
            file: file.take(opened.len()),
            opened,
            path: &full,
        };
        builder.append_data(&mut header, rel_path, reader)?;
    } else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported file type",
        ));
    }
    Ok(())
}

/// A header line, then a tar stream (the guest's `sync apply`).
pub fn apply_stream(root: &Path, input: impl Read) -> io::Result<ApplyReport> {
    let mut input = BufReader::with_capacity(1 << 20, input);
    let mut line = String::new();
    input.read_line(&mut line)?;
    let header: ApplyHeader = serde_json::from_str(line.trim()).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("bad apply header: {e}"))
    })?;
    match header {
        ApplyHeader::Checked { checked } => {
            apply_checked(root, &checked.delete, input, &checked.checks)
        }
        ApplyHeader::GitCopy { git_copy } => git_copy::apply(root, input, &git_copy),
        ApplyHeader::Legacy { delete } => apply(root, &delete, input),
    }
}

/// Remove `delete`, then unpack `tar` under `root`.
pub fn apply(root: &Path, delete: &[String], tar: impl Read) -> io::Result<ApplyReport> {
    apply_inner(root, delete, tar, None)
}

/// Apply only the planned entries and verify each before publication.
pub fn apply_checked(
    root: &Path,
    delete: &[String],
    tar: impl Read,
    checks: &ApplyChecks,
) -> io::Result<ApplyReport> {
    apply_inner(root, delete, tar, Some(checks))
}

const CHECKED_PACK_MAGIC: &[u8; 8] = b"VAPACK3\0";

pub(super) fn pack_checked(
    root: &Path,
    paths: &[String],
    absent: &[String],
    mut out: impl Write,
) -> io::Result<()> {
    let present = present_deletions(root, absent)?;
    out.write_all(CHECKED_PACK_MAGIC)?;
    serde_json::to_writer(&mut out, &present)?;
    out.write_all(b"\n")?;
    pack(root, paths, out)
}

/// Verify that the current worker accepted the checked request before any
/// local mutation, including deletions in a cycle with no incoming files.
pub fn apply_checked_pack(
    root: &Path,
    delete: &[String],
    mut input: impl BufRead,
    mut checks: ApplyChecks,
) -> io::Result<ApplyReport> {
    let mut magic = [0; CHECKED_PACK_MAGIC.len()];
    input.read_exact(&mut magic)?;
    if &magic != CHECKED_PACK_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid sync pack header",
        ));
    }
    let mut response = String::new();
    input.read_line(&mut response)?;
    let present: Vec<String> = serde_json::from_str(&response)?;
    for path in &present {
        if checks.deletes.remove(path).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid skipped deletion",
            ));
        }
    }
    let present_paths: BTreeSet<&str> = present.iter().map(String::as_str).collect();
    let delete: Vec<String> = delete
        .iter()
        .filter(|path| !present_paths.contains(path.as_str()))
        .cloned()
        .collect();
    let mut report = apply_checked(root, &delete, input, &checks)?;
    report.kept_present = present;
    Ok(report)
}

fn apply_inner(
    root: &Path,
    delete: &[String],
    tar: impl Read,
    checks: Option<&ApplyChecks>,
) -> io::Result<ApplyReport> {
    if let Some(checks) = checks {
        let requested: BTreeSet<_> = delete.iter().map(String::as_str).collect();
        if requested.len() != delete.len()
            || !requested
                .into_iter()
                .eq(checks.deletes.keys().map(String::as_str))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "apply deletions differ from the sync plan",
            ));
        }
    }
    fs::create_dir_all(root)?;
    let mut report = ApplyReport::default();
    for rel in delete {
        let result = (|| {
            if let Some(stamp) = checks.and_then(|checks| checks.deletes.get(rel)) {
                check_destination(root, rel, Some(stamp))?;
            }
            remove(root, rel)
        })();
        match result {
            Ok(Removal::Deleted) => report.deleted += 1,
            Ok(Removal::Absent) => {}
            Ok(Removal::KeptNonempty) => report.kept_nonempty.push(rel.clone()),
            Err(error) => report.failed.push((rel.clone(), error.to_string())),
        }
    }
    let mut seen = BTreeMap::new();
    let mut archive = tar::Archive::new(tar);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let rel = String::from_utf8(entry.path_bytes().into_owned()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "archive path is not Unicode")
        })?;
        let result = if let Some(checks) = checks {
            (|| {
                let (path, check) = checks
                    .writes
                    .get_key_value(&rel)
                    .ok_or_else(|| io::Error::other("archive path was not requested"))?;
                let std::collections::btree_map::Entry::Vacant(outcome) = seen.entry(path.as_str())
                else {
                    return Err(io::Error::other("duplicate archive path"));
                };
                outcome.insert(false);
                let mut descendant = path.as_str();
                while let Some((parent, _)) = descendant.rsplit_once('/') {
                    if checks.writes.contains_key(parent) && seen.get(parent) != Some(&true) {
                        return Err(io::Error::other(format!("parent {parent} was not applied")));
                    }
                    descendant = parent;
                }
                write_entry(root, &rel, &mut entry, Some(check))?;
                seen.insert(path.as_str(), true);
                Ok(())
            })()
        } else {
            write_entry(root, &rel, &mut entry, None)
        };
        match result {
            Ok(()) => report.written += 1,
            Err(error) => {
                // Drain what is left of this entry so the stream stays aligned.
                let _ = io::copy(&mut entry, &mut io::sink());
                report.failed.push((rel, error.to_string()));
            }
        }
    }
    if let Some(checks) = checks {
        for path in checks
            .writes
            .keys()
            .filter(|path| !seen.contains_key(path.as_str()))
        {
            report
                .failed
                .push((path.clone(), "path is missing from the archive".into()));
        }
    }
    Ok(report)
}

/// Remove a file or symlink cleanly across Unix and Windows (including read-only
/// files such as `.git/objects/*` and Windows directory symlinks/junctions).
fn remove_file_or_link(full: &Path, meta: &fs::Metadata) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if meta.file_type().is_symlink_dir() {
            return fs::remove_dir(full);
        }
    }
    if meta.is_dir() {
        return fs::remove_dir(full);
    }
    publish::with_writable(full, meta, || fs::remove_file(full))
}

enum Removal {
    Deleted,
    Absent,
    KeptNonempty,
}

fn remove(root: &Path, rel: &str) -> io::Result<Removal> {
    let path = safe_relative(rel).ok_or_else(|| io::Error::other("unsafe path"))?;
    check_parents(root, path)?;
    let full = join_wire_path(root, rel);
    let meta = match fs::symlink_metadata(&full) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Removal::Absent),
        Err(error) => return Err(error),
    };
    if meta.is_dir() && !meta.file_type().is_symlink() {
        // Only an empty folder goes: anything still in it was not ours to remove.
        match fs::remove_dir(&full) {
            Ok(()) => Ok(Removal::Deleted),
            Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => {
                Ok(Removal::KeptNonempty)
            }
            Err(error) => Err(error),
        }
    } else {
        remove_file_or_link(&full, &meta).map(|()| Removal::Deleted)
    }
}

fn check_destination(root: &Path, rel: &str, expected: Option<&Stamp>) -> io::Result<()> {
    let matches = if let Some(expected) = expected {
        scan::stat(root, rel)?.is_some_and(|entry| expected.matches_content(&entry))
    } else {
        let path = safe_relative(rel).ok_or_else(|| io::Error::other("unsafe path"))?;
        check_parents(root, path)?;
        match fs::symlink_metadata(join_wire_path(root, rel)) {
            Ok(_) => false,
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => return Err(error),
        }
    };
    if !matches {
        return Err(io::Error::other("destination changed since the sync scan"));
    }
    Ok(())
}

fn source_changed() -> io::Error {
    io::Error::other("source changed since the sync scan")
}

struct HashingReader<R> {
    inner: R,
    hash: Sha256,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
}

fn write_entry<R: Read>(
    root: &Path,
    rel: &str,
    entry: &mut tar::Entry<R>,
    check: Option<&WriteCheck>,
) -> io::Result<()> {
    let path = safe_relative(rel).ok_or_else(|| io::Error::other("unsafe path in archive"))?;
    // Earlier archive entries can replace a directory with a symlink, so a
    // successful check must never be reused for another entry.
    check_parents(root, path)?;
    let full = join_wire_path(root, rel);
    let parent = full
        .parent()
        .ok_or_else(|| io::Error::other("archive path has no parent"))?;
    let verify_destination = || {
        if let Some(check) = check {
            check_destination(root, rel, check.destination.as_ref())
        } else {
            check_parents(root, path)
        }
    };
    let ensure_parent = || fs::create_dir_all(parent);
    let entry_type = entry.header().entry_type();
    let kind = match entry_type {
        tar::EntryType::Directory => EntryKind::Dir,
        tar::EntryType::Symlink => EntryKind::Symlink,
        tar::EntryType::Regular | tar::EntryType::Continuous => EntryKind::File,
        other => {
            return Err(io::Error::other(format!(
                "unsupported entry type {other:?}"
            )));
        }
    };
    if check.is_some_and(|check| check.source.kind != kind) {
        return Err(source_changed());
    }
    let header = entry.header();
    let mode = header.mode().unwrap_or(0o644) & 0o7777;
    let mtime = std::time::UNIX_EPOCH
        .checked_add(std::time::Duration::from_secs(header.mtime().unwrap_or(0)))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "archive mtime is out of range")
        })?;
    let times = fs::FileTimes::new().set_modified(mtime).set_accessed(mtime);
    let new_path = check.is_some_and(|check| check.destination.is_none());
    match kind {
        EntryKind::Dir => {
            verify_destination()?;
            ensure_parent()?;
            if new_path {
                fs::create_dir(&full)?;
            } else if !fs::symlink_metadata(&full).is_ok_and(|meta| meta.is_dir()) {
                let staged = publish::stage(parent)?;
                crate::fsx::set_mode(staged.path(), mode | 0o700)?;
                verify_destination()?;
                return publish::replace(staged.path(), &full, false);
            }
            crate::fsx::set_mode(&full, mode | 0o700)?;
        }
        EntryKind::Symlink => {
            let target = entry
                .link_name()?
                .ok_or_else(|| io::Error::other("symlink without a target"))?
                .into_owned();
            if let Some(check) = check
                && entry.link_name_bytes().as_deref()
                    != check.source.target.as_deref().map(str::as_bytes)
            {
                return Err(source_changed());
            }
            ensure_parent()?;
            let staging = publish::stage(parent)?;
            let staged = staging.path().join("link");
            crate::fsx::symlink(&target, &staged, &full)?;
            fs::set_times_nofollow(&staged, times)?;
            verify_destination()?;
            publish::replace(&staged, &full, new_path)?;
        }
        EntryKind::File => {
            let size = entry.size();
            let expected_digest = if let Some(check) = check {
                if check.source.size != size {
                    return Err(source_changed());
                }
                Some(
                    check
                        .source
                        .digest
                        .ok_or_else(|| io::Error::other("missing planned source digest"))?,
                )
            } else {
                None
            };
            ensure_parent()?;
            let mut staged = tempfile::Builder::new()
                .prefix(super::STAGED_FILE_PREFIX)
                .suffix(".tmp")
                .tempfile_in(parent)?;
            let written = if let Some(expected) = expected_digest {
                let mut reader = HashingReader {
                    inner: entry,
                    hash: Sha256::new(),
                };
                let written = io::copy(&mut reader, staged.as_file_mut())?;
                let actual: FileDigest = reader.hash.finalize().into();
                if written == size && actual != expected {
                    return Err(source_changed());
                }
                written
            } else {
                io::copy(entry, staged.as_file_mut())?
            };
            if written != size {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "archive file ended before its declared size",
                ));
            }
            crate::fsx::set_mode(staged.path(), mode)?;
            staged.as_file().set_times(times)?;
            verify_destination()?;
            if new_path {
                // Even a file created after the last check is preserved.
                staged
                    .persist_noclobber(&full)
                    .map_err(|error| error.error)?;
            } else {
                publish::replace(staged.path(), &full, false)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn a_directory_symlink_is_staged_before_it_replaces_a_file() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("target")).unwrap();
        let destination = root.path().join("link");
        fs::write(&destination, b"original").unwrap();
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_mtime(1_700_000_000);
        tar.append_link(&mut header, "link", "target").unwrap();
        let report = apply(root.path(), &[], tar.into_inner().unwrap().as_slice()).unwrap();
        if report.failed.is_empty() {
            assert_eq!(fs::read_link(&destination).unwrap(), Path::new("target"));
            assert!(destination.is_dir());
        } else {
            assert_eq!(fs::read(&destination).unwrap(), b"original", "{report:?}");
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }

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
            fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 5)),
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
        assert_eq!(
            crate::fsx::mode_of(&fs::metadata(dst.path().join("bin/run")).unwrap()),
            0o755
        );
        assert_eq!(
            fs::read_link(dst.path().join("link")).unwrap(),
            Path::new("bin/run")
        );
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
        assert_eq!(report.kept_nonempty, ["keep"]);
        assert!(report.failed.is_empty());
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
        pack(src.path(), &["x/evil".into()], &mut stream).unwrap();
        let report = apply(dst.path(), &["x/evil".into()], stream.as_slice()).unwrap();
        assert!(!outside.path().join("evil").exists());
        assert_eq!(report.written, 0);
        assert_eq!(report.failed.len(), 2, "{:?}", report.failed);

        // Sending files must not traverse a symlinked directory either.
        assert!(pack(dst.path(), &["x/evil".into()], io::sink()).is_err());
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
        builder
            .append_data(&mut directory, "dir", io::empty())
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_mtime(0);
        header.set_size(0);
        builder
            .append_link(&mut header, "dir", outside.path())
            .unwrap();
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
                let result = write_entry(dst.path(), "file", &mut entry, None);
                assert_eq!(result.is_err(), fail);
                drop(entry);
                assert!(archive.into_inner().observed);
                if fail && !existing {
                    assert!(!destination.exists());
                } else {
                    assert_eq!(
                        fs::read(&destination).unwrap(),
                        if fail {
                            b"old contents"
                        } else {
                            b"new contents"
                        }
                    );
                }
                assert_eq!(
                    fs::read_dir(dst.path()).unwrap().count(),
                    usize::from(existing || !fail)
                );
            }
        }
    }

    #[test]
    fn pack_fails_on_a_vanished_or_invalid_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, "present during the scan").unwrap();
        let listed = scan::stat(dir.path(), "file").unwrap().unwrap();
        fs::remove_file(path).unwrap();
        let missing = pack(dir.path(), &[listed.path], io::sink()).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        assert!(missing.to_string().starts_with("file:"));
        for rel in ["", "../escape", "a/../escape", "a/./file", "bad\0path"] {
            let invalid = pack(dir.path(), &[rel.into()], io::sink()).unwrap_err();
            assert!(invalid.to_string().contains("unsafe path"));
        }
    }

    #[test]
    fn deletions_require_actual_source_absence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("excluded"), b"still exists").unwrap();
        fs::write(root.join("changed-kind"), b"no children").unwrap();
        let rules = super::super::SyncRules {
            excluded: vec!["excluded".into()],
            ..Default::default()
        };
        assert!(
            !scan::scan(root, &rules, None)
                .unwrap()
                .iter()
                .any(|entry| entry.path == "excluded")
        );
        assert_eq!(
            present_deletions(root, &["excluded".into()]).unwrap(),
            ["excluded"]
        );
        assert!(
            present_deletions(root, &["changed-kind/child".into(), "missing".into()])
                .unwrap()
                .is_empty()
        );
        assert!(present_deletions(&root.join("missing"), &["file".into()]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn pack_fails_on_unsupported_and_unreadable_entries() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(dir.path().join("socket")).unwrap();
        let unsupported = pack(dir.path(), &["socket".into()], io::sink()).unwrap_err();
        assert_eq!(unsupported.kind(), io::ErrorKind::Unsupported);
        if unsafe { libc::getuid() } != 0 {
            let path = dir.path().join("denied");
            fs::write(&path, "blocked").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
            let denied = pack(dir.path(), &["denied".into()], io::sink()).unwrap_err();
            assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied);
        }
    }

    #[test]
    fn pack_fails_if_a_file_shrinks_during_the_transfer() {
        struct Shrink<'a> {
            file: &'a Path,
            tar: &'a mut Vec<u8>,
        }
        impl Write for Shrink<'_> {
            fn write(&mut self, data: &[u8]) -> io::Result<usize> {
                if self.tar.is_empty() {
                    fs::write(self.file, b"x")?;
                }
                self.tar.write(data)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, "original").unwrap();
        let mut tar = Vec::new();
        let error = pack(
            dir.path(),
            &["file".into()],
            Shrink {
                file: &file,
                tar: &mut tar,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("changed while being read"));
        assert!(tar.len() < 512 + "original".len());

        let dst = tempfile::tempdir().unwrap();
        let destination = dst.path().join("file");
        fs::write(&destination, "keep").unwrap();
        if let Ok(report) = apply(dst.path(), &[], tar.as_slice()) {
            assert_eq!(report.written, 0);
        }
        assert_eq!(fs::read(&destination).unwrap(), b"keep");
    }

    #[test]
    fn checked_apply_preserves_edits_made_after_planning_and_reports_missing_payloads() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::write(src.path().join("file"), b"new").unwrap();
        fs::write(dst.path().join("file"), b"old").unwrap();
        let snapshot = |root: &Path| {
            let entry = scan::stat(root, "file").unwrap().unwrap();
            BTreeMap::from([(entry.path.clone(), entry)])
        };
        let src_files = snapshot(src.path());
        let dst_files = snapshot(dst.path());
        let paths = ["file".into()];
        let checks = ApplyChecks::for_paths(&src_files, &dst_files, &paths, &[]).unwrap();

        let mut tar = Vec::new();
        pack(src.path(), &[], &mut tar).unwrap();
        let report = apply_checked(dst.path(), &[], tar.as_slice(), &checks).unwrap();
        assert!(report.failed[0].1.contains("missing from the archive"));

        fs::write(src.path().join("file"), b"now").unwrap();
        tar.clear();
        pack(src.path(), &paths, &mut tar).unwrap();
        let report = apply_checked(dst.path(), &[], tar.as_slice(), &checks).unwrap();
        assert!(report.failed[0].1.contains("source changed"));
        assert_eq!(fs::read(dst.path().join("file")).unwrap(), b"old");

        fs::write(src.path().join("file"), b"new").unwrap();
        fs::write(dst.path().join("file"), b"own").unwrap();
        tar.clear();
        pack(src.path(), &paths, &mut tar).unwrap();
        let report = apply_checked(dst.path(), &[], tar.as_slice(), &checks).unwrap();
        assert!(report.failed[0].1.contains("destination changed"));
        assert_eq!(fs::read(dst.path().join("file")).unwrap(), b"own");

        let delete = ApplyChecks::for_paths(&BTreeMap::new(), &dst_files, &[], &paths).unwrap();
        let report = apply_checked(dst.path(), &paths, io::empty(), &delete).unwrap();
        assert!(report.failed[0].1.contains("destination changed"));
        assert_eq!(fs::read(dst.path().join("file")).unwrap(), b"own");

        fs::write(dst.path().join("file"), b"old").unwrap();
        let report = apply_checked(dst.path(), &[], tar.as_slice(), &checks).unwrap();
        assert_eq!(report.written, 1);
        assert!(report.failed.is_empty());
        assert_eq!(fs::read(dst.path().join("file")).unwrap(), b"new");
    }

    #[test]
    fn checked_apply_replaces_a_file_with_a_directory_before_applying_children() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir(src.path().join("path")).unwrap();
        fs::write(src.path().join("path/file"), b"child").unwrap();
        fs::write(dst.path().join("path"), b"original").unwrap();
        let listing = |root: &Path| {
            scan::scan(root, &Default::default(), None)
                .unwrap()
                .into_iter()
                .map(|entry| (entry.path.clone(), entry))
                .collect()
        };
        let source = listing(src.path());
        let destination = listing(dst.path());
        let paths = ["path".into(), "path/file".into()];
        let checks = ApplyChecks::for_paths(&source, &destination, &paths, &[]).unwrap();
        let mut tar = Vec::new();
        pack(src.path(), &paths, &mut tar).unwrap();

        // Both the parent replacement and its descendant must be refused if
        // an ordinary file is still in the way after the first check fails.
        fs::write(dst.path().join("path"), b"modified").unwrap();
        let report = apply_checked(dst.path(), &[], tar.as_slice(), &checks).unwrap();
        assert_eq!(report.written, 0);
        assert_eq!(report.failed.len(), 2);
        assert_eq!(fs::read(dst.path().join("path")).unwrap(), b"modified");

        // An unrelated replacement directory would let the child pass its
        // own absence check. It still depends on the planned parent write.
        fs::remove_file(dst.path().join("path")).unwrap();
        fs::create_dir(dst.path().join("path")).unwrap();
        let report = apply_checked(dst.path(), &[], tar.as_slice(), &checks).unwrap();
        assert_eq!(report.written, 0);
        assert!(report.failed[1].1.contains("parent path was not applied"));
        assert!(!dst.path().join("path/file").exists());

        fs::remove_dir(dst.path().join("path")).unwrap();
        fs::write(dst.path().join("path"), b"original").unwrap();
        let report = apply_checked(dst.path(), &[], tar.as_slice(), &checks).unwrap();
        assert_eq!(report.written, 2);
        assert!(report.failed.is_empty());
        assert_eq!(fs::read(dst.path().join("path/file")).unwrap(), b"child");
    }

    #[test]
    fn checked_streams_fail_closed_with_legacy_workers() {
        #[derive(Deserialize)]
        struct LegacyApply {
            delete: Vec<String>,
        }
        #[derive(Deserialize)]
        struct LegacyPack {
            paths: Vec<String>,
        }

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file"), b"preserve").unwrap();
        let path = "file".to_owned();
        let entry = scan::stat(dir.path(), &path).unwrap().unwrap();
        let checks = ApplyChecks::for_paths(
            &BTreeMap::new(),
            &BTreeMap::from([(path.clone(), entry)]),
            &[],
            std::slice::from_ref(&path),
        )
        .unwrap();
        let checked = ApplyHeader::new(vec![path.clone()], Some(checks.clone()));
        let checked = serde_json::to_string(&checked).unwrap();
        assert!(serde_json::from_str::<LegacyApply>(&checked).is_err());
        assert!(
            serde_json::from_str::<LegacyApply>(r#"{"delete":[]}"#)
                .unwrap()
                .delete
                .is_empty()
        );

        let pack_request =
            serde_json::to_string(&super::super::PackRequest::new(Vec::new(), true)).unwrap();
        assert!(serde_json::from_str::<LegacyPack>(&pack_request).is_err());
        assert!(
            serde_json::from_str::<LegacyPack>(r#"{"paths":[]}"#)
                .unwrap()
                .paths
                .is_empty()
        );

        let mut tar = Vec::new();
        pack(dir.path(), &[], &mut tar).unwrap();
        let delete = [path];
        assert!(apply_checked_pack(dir.path(), &delete, tar.as_slice(), checks.clone()).is_err());
        assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"preserve");
        tar.clear();
        pack_checked(dir.path(), &[], &delete, &mut tar).unwrap();
        let kept = apply_checked_pack(dir.path(), &delete, tar.as_slice(), checks.clone()).unwrap();
        assert_eq!(kept.kept_present, delete);
        assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"preserve");
        tar.clear();
        let source = tempfile::tempdir().unwrap();
        pack_checked(source.path(), &[], &delete, &mut tar).unwrap();
        let report = apply_checked_pack(dir.path(), &delete, tar.as_slice(), checks).unwrap();
        assert_eq!(report.deleted, 1);
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
        builder
            .append_data(&mut header, "bad-time", &b"x"[..])
            .unwrap();
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
        let old_tmp = dst
            .path()
            .join(format!(".file.{}.vasync-tmp", std::process::id()));
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
        assert_eq!(
            fs::read_to_string(dst.path().join("p")).unwrap(),
            "file now"
        );
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
