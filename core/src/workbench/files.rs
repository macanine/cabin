//! Directory listings and file previews for the panel's file browser.

use std::io::Read;
use std::path::Path;

use base64::Engine as _;

use super::{git, join, relative, resolve};
use crate::error::{Error, Result};
use crate::model::{FsContentKind, FsEntry, FsEntryKind, FsFile, FsListing};

const MAX_ENTRIES: usize = 2000;
const MAX_TEXT: u64 = 1024 * 1024;
const MAX_IMAGE: u64 = 5 * 1024 * 1024;

/// One folder's entries, folders first, then by name (case-insensitive).
/// `.git` is hidden; other dotfiles are listed. Ignored entries are marked.
pub fn list(root: &Path, rel: &str) -> Result<FsListing> {
    let dir = resolve(root, rel)?;
    let real_root = root
        .canonicalize()
        .map_err(|error| Error::path("open", root, error))?;
    let read = std::fs::read_dir(&dir).map_err(|error| Error::path("list", &dir, error))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in read.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == ".git" {
            continue;
        }
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let Ok(kind) = entry.file_type() else { continue };
        let (kind, size) = if kind.is_symlink() {
            // A link to a folder inside the root browses like a folder.
            match entry.path().canonicalize() {
                Ok(target) if target.starts_with(&real_root) && target.is_dir() => (FsEntryKind::Dir, 0),
                _ => (FsEntryKind::Symlink, 0),
            }
        } else if kind.is_dir() {
            (FsEntryKind::Dir, 0)
        } else {
            (FsEntryKind::File, entry.metadata().map(|meta| meta.len()).unwrap_or(0))
        };
        let path = relative(&real_root, &dir.join(&name));
        entries.push(FsEntry { name, path, kind, size, ignored: false });
    }
    entries.sort_by(|a, b| {
        (a.kind != FsEntryKind::Dir)
            .cmp(&(b.kind != FsEntryKind::Dir))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    mark_ignored(root, &mut entries);
    Ok(FsListing { path: relative(&real_root, &dir), entries, truncated })
}

/// Ask git which entries `.gitignore` covers (one `check-ignore` per folder).
fn mark_ignored(root: &Path, entries: &mut [FsEntry]) {
    if entries.is_empty() {
        return;
    }
    let input: String = entries
        .iter()
        .map(|entry| {
            // A trailing slash lets folder-only patterns ("build/") match.
            let slash = if entry.kind == FsEntryKind::Dir { "/" } else { "" };
            format!("{}{slash}\0", entry.path)
        })
        .collect();
    let Some(out) = git::run_input(root, &["check-ignore", "-z", "--stdin"], input.as_bytes(), &[0, 1]) else {
        return;
    };
    let ignored: std::collections::HashSet<&str> = out
        .split('\0')
        .map(|path| path.trim_end_matches('/'))
        .filter(|path| !path.is_empty())
        .collect();
    for entry in entries {
        entry.ignored = ignored.contains(entry.path.as_str());
    }
}

fn image_mime(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        _ => return None,
    })
}

/// A file's contents for preview: text (UTF-8, up to 1 MB), an image (up to
/// 5 MB, base64), or just its size when it is binary or too large.
pub fn read(root: &Path, rel: &str) -> Result<FsFile> {
    join(root, rel)?;
    let path = resolve(root, rel)?;
    let meta = std::fs::metadata(&path).map_err(|error| Error::path("read", &path, error))?;
    if !meta.is_file() {
        return Err(Error::invalid(format!("{rel} is not a file")));
    }
    let size = meta.len();
    let modified_at = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|age| age.as_millis() as i64);
    let mut file = FsFile {
        path: rel.trim_matches('/').to_string(),
        kind: FsContentKind::TooLarge,
        size,
        modified_at,
        text: None,
        base64: None,
        mime_type: None,
    };
    if let Some(mime) = image_mime(rel) {
        if size <= MAX_IMAGE {
            let bytes = std::fs::read(&path).map_err(|error| Error::path("read", &path, error))?;
            file.kind = FsContentKind::Image;
            file.base64 = Some(base64::engine::general_purpose::STANDARD.encode(bytes));
            file.mime_type = Some(mime.into());
        }
        return Ok(file);
    }
    if size > MAX_TEXT {
        return Ok(file);
    }
    let mut bytes = Vec::with_capacity(size as usize);
    std::fs::File::open(&path)
        .and_then(|handle| handle.take(MAX_TEXT).read_to_end(&mut bytes))
        .map_err(|error| Error::path("read", &path, error))?;
    let sniff = &bytes[..bytes.len().min(8192)];
    match (sniff.contains(&0), String::from_utf8(bytes)) {
        (false, Ok(text)) => {
            file.kind = FsContentKind::Text;
            file.text = Some(text);
        }
        _ => file.kind = FsContentKind::Binary,
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_folders_first_and_hides_dot_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("b.txt"), "b").unwrap();
        std::fs::write(root.join("A.md"), "a").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        let listing = list(root, "").unwrap();
        let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["src", "A.md", "b.txt"]);
        let nested = list(root, "src").unwrap();
        assert_eq!(nested.entries[0].path, "src/main.rs");
        assert_eq!(nested.path, "src");
    }

    #[test]
    fn reads_text_and_flags_binary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        std::fs::write(root.join("b.bin"), [0u8, 1, 2]).unwrap();
        std::fs::write(root.join("c.png"), [137u8, 80, 78, 71]).unwrap();
        let text = read(root, "a.txt").unwrap();
        assert_eq!(text.kind, FsContentKind::Text);
        assert_eq!(text.text.as_deref(), Some("hello"));
        assert_eq!(read(root, "b.bin").unwrap().kind, FsContentKind::Binary);
        let image = read(root, "c.png").unwrap();
        assert_eq!(image.kind, FsContentKind::Image);
        assert_eq!(image.mime_type.as_deref(), Some("image/png"));
        assert!(read(root, "../a.txt").is_err());
    }
}
