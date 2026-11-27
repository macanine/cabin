//! Listing a synced tree under its rules.

use std::io::{self, Read, Write};
use std::path::Path;

use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use ts_rs::TS;

use super::{IGNORE_FILE, SyncRules, ignored_patterns};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

/// One path in a tree, as much as change detection needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Relative, `/`-separated.
    pub path: String,
    pub kind: EntryKind,
    #[cfg_attr(test, ts(type = "number"))]
    pub size: u64,
    /// Modification time in nanoseconds since the epoch.
    #[cfg_attr(test, ts(type = "number"))]
    pub mtime_ns: i64,
    pub mode: u32,
    /// A symlink's target, kept as written.
    pub target: Option<String>,
}

/// Is `path` inside one of `scope`'s folders, or on the way to one?
pub fn in_scope(path: &str, scope: Option<&[String]>) -> bool {
    let Some(scope) = scope else { return true };
    scope.iter().any(|dir| {
        dir.is_empty()
            || path == dir
            || path.starts_with(&format!("{dir}/"))
            || dir.starts_with(&format!("{path}/"))
    })
}

/// The tree under `root` that the rules include, sorted by path. With a
/// `scope`, only those folders (and the folders leading to them, so parent
/// `.gitignore` files still apply) are walked.
pub fn scan(
    root: &Path,
    rules: &SyncRules,
    scope: Option<&[String]>,
) -> std::io::Result<Vec<Entry>> {
    let mut overrides = OverrideBuilder::new(root);
    let mut add = |glob: String| {
        overrides
            .add(&glob)
            .map(|_| ())
            .map_err(std::io::Error::other)
    };
    for pattern in ignored_patterns() {
        add(format!("!{pattern}"))?;
    }
    for pattern in rules
        .ignore
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty() && !p.starts_with('#'))
    {
        add(format!("!{pattern}"))?;
    }
    for path in rules
        .excluded
        .iter()
        .map(|p| p.trim().trim_matches('/'))
        .filter(|p| !p.is_empty())
    {
        add(format!("!/{path}"))?;
        add(format!("!/{path}/**"))?;
    }
    // `.git` is only part of a regular scan when it is synced like the rest.
    if rules.git != super::GitHandling::Sync {
        add("!/.git".into())?;
        add("!/.git/**".into())?;
    }
    let overrides = overrides.build().map_err(std::io::Error::other)?;

    let scope_owned: Option<Vec<String>> = scope.map(|dirs| dirs.to_vec());
    let root_owned = root.to_path_buf();
    let mut walker = WalkBuilder::new(root);
    walker
        .standard_filters(false)
        .hidden(false)
        .parents(false)
        .ignore(false)
        .git_global(false)
        .git_ignore(rules.gitignore)
        .git_exclude(rules.gitignore)
        .require_git(false)
        .follow_links(false)
        .overrides(overrides)
        .filter_entry(move |entry| {
            relative(&root_owned, entry.path())
                .is_none_or(|rel| in_scope(&rel, scope_owned.as_deref()))
        });
    walker.add_custom_ignore_filename(IGNORE_FILE);

    let mut entries = Vec::new();
    for item in walker.build() {
        let entry = match item {
            Ok(entry) => entry,
            Err(error) => {
                let kind = error
                    .io_error()
                    .map_or(io::ErrorKind::Other, io::Error::kind);
                if kind == io::ErrorKind::NotFound && error.depth().is_some_and(|depth| depth > 0) {
                    continue;
                }
                // An unreadable subtree is not a deletion on this side.
                return Err(io::Error::new(kind, error));
            }
        };
        if let Some(error) = entry.error().filter(|error| {
            error
                .io_error()
                .is_some_and(|cause| cause.kind() == io::ErrorKind::PermissionDenied)
        }) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                error.clone(),
            ));
        }
        let Some(path) = relative(root, entry.path()) else {
            continue;
        };
        let meta = match std::fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() {
            EntryKind::Symlink
        } else if file_type.is_dir() {
            EntryKind::Dir
        } else if file_type.is_file() {
            EntryKind::File
        } else {
            continue; // sockets, fifos, devices
        };
        entries.push(entry_from(path, kind, &meta, entry.path()));
    }
    entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// One path's entry as it is now, or `None` when it does not exist.
pub fn stat(root: &Path, rel: &str) -> io::Result<Option<Entry>> {
    let full = super::join_wire_path(root, rel);
    let meta = match std::fs::symlink_metadata(&full) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let kind = if meta.file_type().is_symlink() {
        EntryKind::Symlink
    } else if meta.is_dir() {
        EntryKind::Dir
    } else if meta.is_file() {
        EntryKind::File
    } else {
        return Ok(None);
    };
    Ok(Some(entry_from(rel.to_string(), kind, &meta, &full)))
}

fn entry_from(path: String, kind: EntryKind, meta: &std::fs::Metadata, full: &Path) -> Entry {
    Entry {
        path,
        kind,
        size: if kind == EntryKind::File {
            meta.len()
        } else {
            0
        },
        mtime_ns: if kind == EntryKind::Dir {
            0
        } else {
            crate::fsx::mtime_nanos_of(meta) as i64
        },
        mode: crate::fsx::mode_of(meta),
        target: (kind == EntryKind::Symlink)
            .then(|| {
                std::fs::read_link(full)
                    .ok()
                    .map(|t| t.to_string_lossy().into_owned())
            })
            .flatten(),
    }
}

pub(super) fn wire_path(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    // Entries and watcher scopes share `/` separators on the wire.
    Some(if std::path::MAIN_SEPARATOR == '/' {
        text.to_owned()
    } else {
        text.replace(std::path::MAIN_SEPARATOR, "/")
    })
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    let text = wire_path(path.strip_prefix(root).ok()?)?;
    (!text.is_empty()).then_some(text)
}

/// Binary listing used by `cabind sync scan` for large trees. JSON
/// serialization was a visible part of every Two-way cycle; this form keeps
/// the same entries with a fixed header and length-prefixed strings.
pub const BINARY_MAGIC: &[u8; 8] = b"VASCAN1\0";
const MAX_BINARY_PATH: u32 = 1 << 20;
const MAX_BINARY_RECORDS: u32 = 10_000_000;

pub fn read_binary(mut input: impl Read) -> io::Result<Vec<Entry>> {
    let mut magic = [0; 8];
    input.read_exact(&mut magic)?;
    if &magic != BINARY_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a sync scan listing",
        ));
    }
    let count = read_u32(&mut input)?;
    if count > MAX_BINARY_RECORDS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sync scan listing is too large",
        ));
    }
    // The count is supplied by the other process. Allocate as records arrive,
    // rather than trusting a short or corrupt listing to reserve its full size.
    let mut entries = Vec::new();
    for _ in 0..count {
        let path = read_string(&mut input)?;
        if super::safe_relative(&path).is_none() || path.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid sync entry path",
            ));
        }
        let mut kind = [0];
        input.read_exact(&mut kind)?;
        let kind = match kind[0] {
            0 => EntryKind::File,
            1 => EntryKind::Dir,
            2 => EntryKind::Symlink,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid sync entry kind",
                ));
            }
        };
        let size = read_u64(&mut input)?;
        let mtime_ns = read_u64(&mut input)? as i64;
        let mode = read_u32(&mut input)?;
        let target = read_optional_string(&mut input)?;
        entries.push(Entry {
            path,
            kind,
            size,
            mtime_ns,
            mode,
            target,
        });
    }
    let mut trailing = [0; 1];
    match input.read_exact(&mut trailing) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(entries),
        Err(error) => Err(error),
        Ok(()) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing data in sync scan listing",
        )),
    }
}

pub fn write_binary(entries: &[Entry], mut out: impl Write) -> io::Result<()> {
    out.write_all(BINARY_MAGIC)?;
    let count =
        u32::try_from(entries.len()).map_err(|_| io::Error::other("too many sync entries"))?;
    write_u32(&mut out, count)?;
    for entry in entries {
        write_string(&mut out, &entry.path)?;
        out.write_all(&[match entry.kind {
            EntryKind::File => 0,
            EntryKind::Dir => 1,
            EntryKind::Symlink => 2,
        }])?;
        write_u64(&mut out, entry.size)?;
        write_u64(&mut out, entry.mtime_ns as u64)?;
        write_u32(&mut out, entry.mode)?;
        write_optional_string(&mut out, entry.target.as_deref())?;
    }
    Ok(())
}

fn read_string(input: &mut impl Read) -> io::Result<String> {
    let len = read_u32(input)?;
    if len > MAX_BINARY_PATH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sync path is too long",
        ));
    }
    let mut bytes = vec![0; len as usize];
    input.read_exact(&mut bytes)?;
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "sync path is not UTF-8"))
}

fn write_string(out: &mut impl Write, value: &str) -> io::Result<()> {
    let bytes = value.as_bytes();
    let len = u32::try_from(bytes.len()).map_err(|_| io::Error::other("sync path is too long"))?;
    write_u32(out, len)?;
    out.write_all(bytes)
}

fn read_optional_string(input: &mut impl Read) -> io::Result<Option<String>> {
    let mut present = [0];
    input.read_exact(&mut present)?;
    match present[0] {
        0 => Ok(None),
        1 => read_string(input).map(Some),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid sync target flag",
        )),
    }
}

fn write_optional_string(out: &mut impl Write, value: Option<&str>) -> io::Result<()> {
    match value {
        Some(value) => {
            out.write_all(&[1])?;
            write_string(out, value)
        }
        None => out.write_all(&[0]),
    }
}

fn read_u32(input: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(input: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn write_u32(out: &mut impl Write, value: u32) -> io::Result<()> {
    out.write_all(&value.to_le_bytes())
}

fn write_u64(out: &mut impl Write, value: u64) -> io::Result<()> {
    out.write_all(&value.to_le_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (path, body) in [
            ("src/main.rs", "fn main() {}"),
            ("src/gen/out.rs", "x"),
            ("node_modules/a/index.js", "x"),
            ("build/app", "x"),
            ("notes.log", "x"),
            ("docs/readme.md", "x"),
            (".DS_Store", "x"),
            (".gitignore", "node_modules/\n"),
            ("src/.gitignore", "gen/\n"),
            (".cabinignore", "build/\n"),
        ] {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("src/main.rs", root.join("link")).unwrap();
        dir
    }

    fn paths(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn honours_gitignore_cabinignore_patterns_and_unticked_paths() {
        let dir = tree();
        std::fs::create_dir_all(dir.path().join(".git/refs")).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "ref").unwrap();
        let rules = SyncRules {
            gitignore: true,
            ignore: vec!["*.log".into()],
            excluded: vec!["docs".into()],
            ..SyncRules::default()
        };
        let entries = scan(dir.path(), &rules, None).unwrap();
        // The fixture adds a symlink only where the host can create one.
        let expected: Vec<&str> = if cfg!(unix) {
            vec![
                ".cabinignore",
                ".gitignore",
                "link",
                "src",
                "src/.gitignore",
                "src/main.rs",
            ]
        } else {
            vec![
                ".cabinignore",
                ".gitignore",
                "src",
                "src/.gitignore",
                "src/main.rs",
            ]
        };
        assert_eq!(paths(&entries), expected);
        if cfg!(unix) {
            let link = entries.iter().find(|e| e.path == "link").unwrap();
            assert_eq!(
                (link.kind, link.target.as_deref()),
                (EntryKind::Symlink, Some("src/main.rs"))
            );
        }
        assert_eq!(
            entries
                .iter()
                .find(|e| e.path == "src/main.rs")
                .unwrap()
                .size,
            12
        );
    }

    #[test]
    fn missing_scan_root_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("missing");
        assert_eq!(
            scan(&root, &SyncRules::default(), None).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[cfg(unix)]
    #[test]
    fn denied_subtree_does_not_look_like_deleted_files() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::getuid() } == 0 {
            return;
        }
        let dir = tree();
        let blocked = dir.path().join("src");
        let permissions = blocked.metadata().unwrap().permissions();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let listing = scan(dir.path(), &SyncRules::default(), None);
        let entry = stat(dir.path(), "src/main.rs");
        std::fs::set_permissions(&blocked, permissions).unwrap();
        assert_eq!(listing.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(entry.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(stat(dir.path(), "deleted").unwrap().is_none());
    }

    #[test]
    fn binary_listing_round_trips_and_rejects_garbage() {
        let entries = vec![
            Entry {
                path: "src/main.rs".into(),
                kind: EntryKind::File,
                size: 12,
                mtime_ns: 1_700_000_000_123_456_789,
                mode: 0o644,
                target: None,
            },
            Entry {
                path: "link".into(),
                kind: EntryKind::Symlink,
                size: 0,
                mtime_ns: 1,
                mode: 0o777,
                target: Some("src/main.rs".into()),
            },
            Entry {
                path: "src".into(),
                kind: EntryKind::Dir,
                size: 0,
                mtime_ns: 0,
                mode: 0o755,
                target: None,
            },
        ];
        let mut bytes = Vec::new();
        write_binary(&entries, &mut bytes).unwrap();
        assert!(bytes.starts_with(BINARY_MAGIC));
        assert_eq!(read_binary(bytes.as_slice()).unwrap(), entries);
        assert!(read_binary(&b"nope"[..]).is_err());
    }

    #[test]
    fn binary_listing_rejects_truncation_and_malformed_fields() {
        let entry = Entry {
            path: "file".into(),
            kind: EntryKind::Symlink,
            size: 0,
            mtime_ns: -1,
            mode: 0o777,
            target: Some("target".into()),
        };
        let mut bytes = Vec::new();
        write_binary(std::slice::from_ref(&entry), &mut bytes).unwrap();
        for end in 0..bytes.len() {
            assert!(
                read_binary(&bytes[..end]).is_err(),
                "accepted truncation at {end}"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            read_binary(trailing.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        for (offset, value) in [(16, 0xff), (20, 3), (41, 2)] {
            let mut malformed = bytes.clone();
            malformed[offset] = value;
            assert!(
                read_binary(malformed.as_slice()).is_err(),
                "accepted malformed byte at {offset}"
            );
        }
        for path in ["", "../escape", "/absolute", "bad\0path"] {
            let mut malformed = Vec::new();
            write_binary(
                &[Entry {
                    path: path.into(),
                    ..entry.clone()
                }],
                &mut malformed,
            )
            .unwrap();
            assert!(
                read_binary(malformed.as_slice()).is_err(),
                "accepted path {path:?}"
            );
        }
        for count in [MAX_BINARY_RECORDS, MAX_BINARY_RECORDS + 1] {
            let mut short = BINARY_MAGIC.to_vec();
            short.extend_from_slice(&count.to_le_bytes());
            assert!(read_binary(short.as_slice()).is_err());
        }
        let mut long_path = BINARY_MAGIC.to_vec();
        long_path.extend_from_slice(&1u32.to_le_bytes());
        long_path.extend_from_slice(&(MAX_BINARY_PATH + 1).to_le_bytes());
        assert_eq!(
            read_binary(long_path.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn binary_listing_accepts_short_reads() {
        struct ShortReads<R>(R);
        impl<R: Read> Read for ShortReads<R> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let len = buf.len().min(3);
                self.0.read(&mut buf[..len])
            }
        }
        let entry = Entry {
            path: "dir/file".into(),
            kind: EntryKind::File,
            size: 42,
            mtime_ns: -1,
            mode: 0o644,
            target: None,
        };
        let mut bytes = Vec::new();
        write_binary(std::slice::from_ref(&entry), &mut bytes).unwrap();
        assert_eq!(read_binary(ShortReads(bytes.as_slice())).unwrap(), [entry]);
    }

    #[test]
    fn gitignore_can_be_turned_off() {
        let dir = tree();
        let rules = SyncRules {
            gitignore: false,
            ..SyncRules::default()
        };
        let entries = scan(dir.path(), &rules, None).unwrap();
        assert!(paths(&entries).contains(&"node_modules/a/index.js"));
        assert!(paths(&entries).contains(&"src/gen/out.rs"));
        // The project's own ignore file always applies.
        assert!(!paths(&entries).contains(&"build/app"));
        assert!(!paths(&entries).contains(&".DS_Store"));
    }

    #[test]
    fn git_is_scanned_only_when_synced_or_on_its_own() {
        let dir = tree();
        std::fs::create_dir_all(dir.path().join(".git/refs")).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "ref").unwrap();
        let default = scan(dir.path(), &SyncRules::default(), None).unwrap();
        assert!(
            !paths(&default)
                .iter()
                .any(|p| p.starts_with(".git/") || *p == ".git")
        );
        let synced = SyncRules {
            git: crate::sync::GitHandling::Sync,
            ..SyncRules::default()
        };
        assert!(paths(&scan(dir.path(), &synced, None).unwrap()).contains(&".git/HEAD"));
        let git_scope = vec![".git".to_string()];
        let only = scan(dir.path(), &SyncRules::git_only(), Some(&git_scope)).unwrap();
        assert_eq!(paths(&only), [".git", ".git/HEAD", ".git/refs"]);
    }

    #[test]
    fn a_scope_walks_only_those_folders_with_parent_rules_still_applied() {
        let dir = tree();
        let scope = vec!["src".to_string()];
        let entries = scan(dir.path(), &SyncRules::default(), Some(&scope)).unwrap();
        assert_eq!(paths(&entries), ["src", "src/.gitignore", "src/main.rs"]);
        assert!(in_scope("src/a/b", Some(&scope)));
        assert!(in_scope("", Some(&[String::new()])));
        assert!(!in_scope("docs/x", Some(&scope)));
    }
}
