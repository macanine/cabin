//! Publish the CopyOnce repository only after the entire upload succeeds.

use std::fs;
use std::io::{self, Read};
use std::path::Path;

use super::{ApplyChecks, ApplyReport, apply_checked};
use crate::sync::scan::EntryKind;

pub(super) fn apply(
    root: &Path,
    mut input: impl Read,
    checks: &ApplyChecks,
) -> io::Result<ApplyReport> {
    if !checks.deletes.is_empty()
        || !checks
            .writes
            .get(".git")
            .is_some_and(|check| check.source.kind == EntryKind::Dir)
        || checks.writes.iter().any(|(path, check)| {
            (path != ".git" && !path.starts_with(".git/")) || check.destination.is_some()
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid .git copy plan",
        ));
    }

    let destination = root.join(".git");
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                ".git already exists",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::create_dir_all(root)?;
    let staging = tempfile::Builder::new()
        .prefix(&format!("{}git-", crate::sync::STAGED_FILE_PREFIX))
        .suffix(".tmp")
        .tempdir_in(root)?;
    let mut report = apply_checked(staging.path(), &[], &mut input, checks)?;
    if !report.failed.is_empty() {
        report.written = 0;
        return Ok(report);
    }

    // The tar reader stops at the first zero block (or plain EOF). The next
    // zero block proves the sender finished packing every requested path.
    let mut end = [0; 512];
    input.read_exact(&mut end)?;
    if end.iter().any(|byte| *byte != 0) || input.read(&mut [0])? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid .git archive ending",
        ));
    }
    super::publish::rename_new(&staging.path().join(".git"), &destination)?;
    Ok(report)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use crate::sync::{
        SyncRules,
        archive::{ApplyHeader, apply_stream, pack},
        scan,
    };
    use std::collections::BTreeMap;

    fn fixture() -> (tempfile::TempDir, Vec<String>, ApplyHeader) {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join(".git/objects")).unwrap();
        fs::write(source.path().join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        fs::write(source.path().join(".git/config"), b"configuration\n").unwrap();
        let scope = [".git".into()];
        let listing: BTreeMap<_, _> = scan(source.path(), &SyncRules::git_only(), Some(&scope))
            .unwrap()
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        let paths: Vec<_> = listing.keys().cloned().collect();
        let checks = ApplyChecks::for_paths(&listing, &BTreeMap::new(), &paths, &[]).unwrap();
        (source, paths, ApplyHeader::git_copy(checks))
    }

    fn request(source: &Path, paths: &[String], header: &ApplyHeader) -> Vec<u8> {
        let mut stream = serde_json::to_vec(header).unwrap();
        stream.push(b'\n');
        pack(source, paths, &mut stream).unwrap();
        stream
    }

    #[test]
    fn a_partial_copy_leaves_no_git_directory_and_can_retry() {
        let (source, paths, header) = fixture();
        let root = tempfile::tempdir().unwrap();
        let only_head = request(source.path(), &paths[..2], &header);
        let report = apply_stream(root.path(), only_head.as_slice()).unwrap();
        assert_eq!(report.written, 0);
        assert!(report.failed.iter().any(|(path, _)| path == ".git/config"));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);

        let full = request(source.path(), &paths, &header);
        assert!(apply_stream(root.path(), &full[..full.len() - 512]).is_err());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);

        fs::write(source.path().join(".git/config"), b"configuration!").unwrap();
        let stale = request(source.path(), &paths, &header);
        let report = apply_stream(root.path(), stale.as_slice()).unwrap();
        assert_eq!(report.written, 0);
        assert!(report.failed[0].1.contains("source changed"));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);

        let report = apply_stream(root.path(), full.as_slice()).unwrap();
        assert!(report.failed.is_empty());
        assert_eq!(report.written, paths.len() as u64);
        assert_eq!(
            fs::read(root.path().join(".git/config")).unwrap(),
            b"configuration\n"
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn publishes_without_replacing_even_an_empty_concurrent_git_directory() {
        let root = tempfile::tempdir().unwrap();
        let staged = root.path().join("staged");
        let destination = root.path().join(".git");
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("HEAD"), b"staged").unwrap();
        fs::create_dir(&destination).unwrap();
        let error = super::super::publish::rename_new(&staged, &destination).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(!destination.join("HEAD").exists());
    }

    #[test]
    fn refuses_an_existing_guest_repository() {
        let (source, paths, header) = fixture();
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::write(root.path().join(".git/HEAD"), b"existing").unwrap();
        let stream = request(source.path(), &paths, &header);
        let error = apply_stream(root.path(), stream.as_slice()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(root.path().join(".git/HEAD")).unwrap(),
            b"existing"
        );
    }
}
