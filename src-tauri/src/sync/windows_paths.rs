//! Guard the Windows destination before a two-way sync can mutate either side.
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::Path;

#[cfg(windows)]
fn compare(a: &[u16], b: &[u16]) -> Option<Ordering> {
    use windows_sys::Win32::Globalization::CompareStringOrdinal;
    // The slices are valid for their explicit lengths; Windows does not retain them.
    match unsafe {
        CompareStringOrdinal(
            a.as_ptr(),
            a.len().try_into().ok()?,
            b.as_ptr(),
            b.len().try_into().ok()?,
            1,
        )
    } {
        1 => Some(Ordering::Less),
        2 => Some(Ordering::Equal),
        3 => Some(Ordering::Greater),
        _ => None,
    }
}

#[cfg(not(windows))]
fn compare(a: &[u16], b: &[u16]) -> Option<Ordering> {
    // Only used for portable ASCII fixtures. Windows uses its own ordinal comparison.
    let fold = |unit: &u16| {
        if *unit <= 127 {
            (*unit as u8).to_ascii_uppercase() as u16
        } else {
            *unit
        }
    };
    Some(a.iter().map(fold).cmp(b.iter().map(fold)))
}

/// Windows reserves DOS device names regardless of case or file extension
/// (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, `con.txt`, etc.).
fn is_reserved_windows_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).trim_end();
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "CONIN$"
            | "CONOUT$"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "COM¹"
            | "COM²"
            | "COM³"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
            | "LPT¹"
            | "LPT²"
            | "LPT³"
    )
}

/// Reject distinct spellings that Windows can resolve to one destination.
/// Include implicit parents, even if a scan or ignore rule omitted them.
pub(super) fn check_names<'a>(paths: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let mut names = BTreeSet::new();
    for path in paths {
        if path.split('/').any(|part| {
            part.is_empty()
                || part.ends_with(['.', ' '])
                || is_reserved_windows_name(part)
                || part.chars().any(|ch| {
                    ch < ' ' || matches!(ch, '\\' | ':' | '"' | '<' | '>' | '|' | '?' | '*')
                })
        }) {
            return Err(format!(
                "Sync stopped: {path:?} cannot be safely synced to Windows. Rename or exclude it."
            ));
        }
        let mut prefix = path;
        while names.insert(prefix) {
            let Some((parent, _)) = prefix.rsplit_once('/') else {
                break;
            };
            prefix = parent;
        }
    }
    let mut names: Vec<_> = names
        .into_iter()
        .map(|name| (name.encode_utf16().collect::<Vec<_>>(), name))
        .collect();
    let mut comparison_failed = false;
    names.sort_unstable_by(|(a, _), (b, _)| {
        compare(a, b).unwrap_or_else(|| {
            comparison_failed = true;
            a.cmp(b)
        })
    });
    if comparison_failed {
        return Err("Sync stopped: Windows could not compare the project paths safely.".into());
    }
    for ((a, first), (b, second)) in names.iter().zip(names.iter().skip(1)) {
        match compare(a, b) {
            Some(Ordering::Equal) => {
                return Err(format!(
                    "Sync stopped: {first:?} and {second:?} can refer to the same Windows path. Rename one to a distinct name, or exclude it."
                ));
            }
            None => {
                return Err(
                    "Sync stopped: Windows could not compare the project paths safely.".into(),
                );
            }
            _ => {}
        }
    }
    Ok(())
}

/// A missing scan entry is not permission to delete its guest counterpart
/// or overwrite it from the guest. Probe the actual Windows path first.
pub(super) fn require_local_absence(root: &Path, path: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(agent_core::sync::join_wire_path(root, path)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(format!(
            "Sync stopped: {path:?} resolves to a local item that was not in the scan. Check its name or ignore rules."
        )),
        Err(error) => Err(format!(
            "Sync stopped: could not confirm that {path:?} is absent locally: {error}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_two_pulls_and_a_missing_spelling_from_an_old_base() {
        let guest_paths = ["README.txt", "Readme.txt"];
        assert!(
            check_names(guest_paths)
                .unwrap_err()
                .contains("same Windows path")
        );
        use agent_core::sync::SyncMode;
        use agent_core::sync::plan::{Action, next_base, reconcile};
        use agent_core::sync::scan::{Entry, EntryKind};
        use std::collections::BTreeMap;

        let guest: BTreeMap<_, _> = guest_paths
            .into_iter()
            .zip([7, 9])
            .map(|(path, size)| {
                (
                    path.to_string(),
                    Entry {
                        path: path.into(),
                        kind: EntryKind::File,
                        size,
                        mtime_ns: 0,
                        mode: 0o644,
                        target: None,
                        digest: Some([size as u8; 32]),
                    },
                )
            })
            .collect();
        let empty_base = BTreeMap::new();
        let empty_local = BTreeMap::new();
        let pulls = reconcile(SyncMode::TwoWay, &empty_base, &empty_local, &guest);
        assert_eq!(pulls.len(), 2);
        assert!(pulls.iter().all(|action| matches!(action, Action::Pull(_))));
        let old_base = next_base(&empty_base, &empty_local, &guest, &pulls);
        // An old version could have accepted both pulls into a single file.
        let local = BTreeMap::from([("README.txt".into(), guest["README.txt"].clone())]);
        assert_eq!(
            reconcile(SyncMode::TwoWay, &old_base, &local, &guest),
            [Action::DeleteVm("Readme.txt".into())],
        );
        assert!(check_names(local.keys().chain(guest.keys()).map(String::as_str)).is_err());
        // Renaming to a genuinely distinct name makes future cycles safe.
        assert!(check_names(["README.txt", "README.txt", "Readme-old.txt"]).is_ok());
    }

    #[test]
    fn catches_parent_collisions_and_case_only_renames() {
        assert!(
            check_names(["Src/a.txt", "src/b.txt"])
                .unwrap_err()
                .contains("\"Src\"")
        );
        assert!(check_names(["doc.txt", "DOC.txt"]).is_err());
        assert!(check_names(["Src", "Src/a.txt", "Src/b.txt", "Other/a.txt"]).is_ok());
    }

    #[test]
    fn refuses_paths_win32_can_reinterpret() {
        for path in [
            "name.",
            "name ",
            "src./file",
            "dir\\file",
            "file:stream",
            "../file",
            "CON",
            "conin$",
            "CONOUT$",
            "nul.json",
            "src/com1.txt",
            "src/com¹.txt",
            "LPT9",
            "lpt³",
            "aux/readme.md",
        ] {
            assert!(check_names([path]).is_err(), "accepted {path}");
        }
    }

    #[test]
    fn checks_actual_absence_even_for_an_unlisted_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Existing.txt"), "untouched").unwrap();
        assert!(require_local_absence(dir.path(), "Existing.txt").is_err());
        assert!(require_local_absence(dir.path(), "NotThere.txt").is_ok());
        #[cfg(windows)]
        if dir.path().join("existing.txt").exists() {
            assert!(require_local_absence(dir.path(), "existing.txt").is_err());
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Existing.txt")).unwrap(),
            "untouched"
        );
    }
}
