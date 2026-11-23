//! Allocated storage and cache cleanup for the desktop's single VM.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::machine::NAME;
use crate::paths::StatePaths;

#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum StorageKind {
    Application,
    VirtualDisk,
    GuestFiles,
    SystemImages,
    Engine,
    Downloads,
    Logs,
    Settings,
    WebkitCache,
    WebkitData,
    OtherData,
    DeveloperCache,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct StorageEntry {
    pub kind: StorageKind,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub bytes: u64,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub cleanable_bytes: u64,
    pub complete: bool,
}

impl StorageEntry {
    fn new(kind: StorageKind) -> Self {
        Self {
            kind,
            bytes: 0,
            cleanable_bytes: 0,
            complete: true,
        }
    }
}

#[derive(Default)]
pub struct Inventory {
    entries: BTreeMap<StorageKind, StorageEntry>,
    hard_links: HashSet<(u64, u64)>,
}

impl Inventory {
    fn entry(&mut self, kind: StorageKind) -> &mut StorageEntry {
        self.entries
            .entry(kind)
            .or_insert_with(|| StorageEntry::new(kind))
    }

    fn walk(
        &mut self,
        root: &Path,
        kind: StorageKind,
        classify: impl Fn(&Path, StorageKind) -> StorageKind,
        exclude: Option<&Path>,
    ) {
        // Keep one directory iterator per level. Large developer caches can
        // have many sibling files and should not need a stack entry per file.
        struct Directory {
            children: fs::ReadDir,
            kind: StorageKind,
        }
        self.entry(kind);
        let mut stack: Vec<Directory> = Vec::new();
        let mut device = None;
        let mut next = Some((root.to_path_buf(), kind));
        loop {
            let (path, kind) = match next.take() {
                Some(item) => item,
                None => {
                    let Some(dir) = stack.last_mut() else { break };
                    match dir.children.next() {
                        Some(Ok(item)) => (item.path(), dir.kind),
                        Some(Err(_)) => {
                            let kind = dir.kind;
                            self.entry(kind).complete = false;
                            continue;
                        }
                        None => {
                            stack.pop();
                            continue;
                        }
                    }
                }
            };
            if Some(path.as_path()) == exclude {
                continue;
            }
            let kind = classify(&path, kind);
            let meta = match fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => {
                    self.entry(kind).complete = false;
                    continue;
                }
            };
            if device_id(&meta) != *device.get_or_insert(device_id(&meta)) {
                self.entry(kind).complete = false;
                continue;
            }
            if !meta.is_file()
                || link_count(&meta) < 2
                || self.hard_links.insert(file_id(&meta).unwrap_or((0, 0)))
            {
                let entry = self.entry(kind);
                entry.bytes = entry.bytes.saturating_add(allocated_bytes(&meta));
            }
            if meta.is_dir() {
                match fs::read_dir(path) {
                    Ok(children) => stack.push(Directory { children, kind }),
                    Err(_) => self.entry(kind).complete = false,
                }
            }
        }
    }

    pub fn measure(&mut self, path: &Path, kind: StorageKind, exclude: Option<&Path>) {
        self.walk(path, kind, |_, kind| kind, exclude);
    }

    pub fn measure_vm(&mut self, home: &Path) {
        use StorageKind::*;
        let paths = StatePaths::new(home);
        let disk = paths.instance(NAME).disk();
        let archive = crate::qemu_engine::archive_path(home);
        let staging = crate::qemu_engine::staging_path(home);
        let archive_partial = crate::download::partial_path(&archive);
        for kind in [
            VirtualDisk,
            GuestFiles,
            SystemImages,
            Engine,
            Downloads,
            Logs,
            Settings,
        ] {
            self.entry(kind);
        }
        self.walk(
            home,
            OtherData,
            |path, kind| {
                if path == disk {
                    return VirtualDisk;
                }
                if kind == Engine && (path == staging || path == archive_partial || path == archive)
                {
                    return Downloads;
                }
                if kind == SystemImages && image_partial(path) {
                    return Downloads;
                }
                if path.parent() == Some(home) {
                    return match path.file_name().and_then(|name| name.to_str()) {
                        Some("instances") => GuestFiles,
                        Some("images") => SystemImages,
                        Some("engines") => Engine,
                        Some("logs" | "destroy.log") => Logs,
                        Some("network" | "sync" | "state" | "proxy" | "journal.jsonl") => Settings,
                        _ => kind,
                    };
                }
                kind
            },
            None,
        );
    }

    pub fn finish(mut self, home: &Path) -> Vec<StorageEntry> {
        for (kind, target) in [
            (StorageKind::Downloads, CleanupTarget::Downloads),
            (StorageKind::Logs, CleanupTarget::Logs),
        ] {
            match candidates(home, target, SystemTime::now()) {
                Ok(paths) => {
                    let estimate = candidate_usage(&paths, kind);
                    let row = self.entry(kind);
                    row.cleanable_bytes = estimate.bytes;
                    row.complete &= estimate.complete;
                }
                Err(_) => self.entry(kind).complete = false,
            }
        }
        self.entries.into_values().collect()
    }
}

/// File identity and allocation: Unix inodes and blocks, and the coarser
/// facts Windows metadata offers.
#[cfg(unix)]
mod native {
    use std::fs::Metadata;
    use std::os::unix::fs::MetadataExt;

    pub fn device_id(meta: &Metadata) -> u64 {
        meta.dev()
    }
    pub fn link_count(meta: &Metadata) -> u64 {
        meta.nlink()
    }
    pub fn file_id(meta: &Metadata) -> Option<(u64, u64)> {
        Some((meta.dev(), meta.ino()))
    }
    pub fn changed_time(meta: &Metadata) -> Option<(i64, i64)> {
        Some((meta.ctime(), meta.ctime_nsec()))
    }
    pub fn allocated_bytes(meta: &Metadata) -> u64 {
        meta.blocks().saturating_mul(512)
    }
}

#[cfg(not(unix))]
mod native {
    use std::fs::Metadata;

    pub fn device_id(_: &Metadata) -> u64 {
        0
    }
    pub fn link_count(_: &Metadata) -> u64 {
        1
    }
    pub fn file_id(_: &Metadata) -> Option<(u64, u64)> {
        None
    }
    pub fn changed_time(_: &Metadata) -> Option<(i64, i64)> {
        None
    }
    pub fn allocated_bytes(meta: &Metadata) -> u64 {
        meta.len()
    }
}

pub use native::allocated_bytes;
pub(crate) use native::{changed_time, device_id, file_id, link_count};

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct StorageVolume {
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub total_bytes: u64,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub available_bytes: u64,
}

pub fn volume(path: &Path) -> Option<StorageVolume> {
    Some(StorageVolume {
        total_bytes: fs4::total_space(path).ok()?,
        available_bytes: fs4::available_space(path).ok()?,
    })
}

#[derive(Clone, Copy, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum CleanupTarget {
    Downloads,
    Logs,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct CleanupResult {
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub removed_bytes: u64,
    pub failures: Vec<String>,
}

pub const LOG_RETENTION_DAYS: u64 = 30;

fn image_partial(path: &Path) -> bool {
    let suffix = format!("-minimal-cloudimg-{}.img.part", crate::arch::GuestArch::current().cloud_image_suffix());
    path.file_name().and_then(|name| name.to_str()).is_some_and(|name| {
        name.strip_suffix(&suffix).is_some_and(|release| {
            release
                .strip_prefix("ubuntu-")
                .is_some_and(|version| !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.'))
        })
    })
}

fn real_directory(path: &Path) -> io::Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(Some(meta)),
        Ok(_) => Err(io::Error::other(format!(
            "Expected a real directory at {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn candidates(home: &Path, target: CleanupTarget, now: SystemTime) -> io::Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    let Some(root) = real_directory(home)? else {
        return Ok(result);
    };
    let paths = StatePaths::new(home);
    let same_volume = |path: &Path| -> io::Result<bool> {
        Ok(real_directory(path)?.is_some_and(|meta| device_id(&meta) == device_id(&root)))
    };
    match target {
        CleanupTarget::Downloads => {
            if same_volume(&paths.images())? {
                for row in fs::read_dir(paths.images())? {
                    let path = row?.path();
                    if image_partial(&path) && fs::symlink_metadata(&path)?.is_file() {
                        result.push(path)
                    }
                }
            }
            if same_volume(&paths.engines())? {
                let archive = crate::qemu_engine::archive_path(home);
                let staging = crate::qemu_engine::staging_path(home);
                if fs::symlink_metadata(&staging)
                    .is_ok_and(|meta| meta.is_dir() && device_id(&meta) == device_id(&root))
                {
                    result.push(staging);
                }
                let partial = crate::download::partial_path(&archive);
                if fs::symlink_metadata(&partial).is_ok_and(|meta| meta.is_file()) {
                    result.push(partial);
                }
                if cfg!(target_os = "macos")
                    && crate::qemu_engine::installed(home).is_some()
                    && fs::symlink_metadata(&archive).is_ok_and(|meta| meta.is_file())
                {
                    result.push(archive);
                }
            }
        }
        CleanupTarget::Logs => {
            let retention = Duration::from_secs(LOG_RETENTION_DAYS * 86_400);
            if same_volume(&paths.logs())? {
                for dir in [paths.instance_logs(NAME), paths.instance_logs("global")] {
                    if !same_volume(&dir)? {
                        continue;
                    }
                    for row in fs::read_dir(dir)? {
                        let row = row?;
                        let path = row.path();
                        let meta = fs::symlink_metadata(&path)?;
                        if meta.is_file()
                            && path.extension().is_some_and(|ext| ext == "log")
                            && meta
                                .modified()
                                .ok()
                                .and_then(|at| now.duration_since(at).ok())
                                .is_some_and(|age| age >= retention)
                        {
                            result.push(path);
                        }
                    }
                }
            }
        }
    }
    Ok(result)
}

fn candidate_usage(paths: &[PathBuf], kind: StorageKind) -> StorageEntry {
    let mut scan = Inventory::default();
    for path in paths {
        scan.measure(path, kind, None);
    }
    scan.entries
        .remove(&kind)
        .unwrap_or_else(|| StorageEntry::new(kind))
}

/// Call with the machine operation lock held and the VM stopped.
pub(crate) fn clean(home: &Path, target: CleanupTarget) -> crate::Result<CleanupResult> {
    let mut result = CleanupResult {
        removed_bytes: 0,
        failures: Vec::new(),
    };
    let candidates = candidates(home, target, SystemTime::now())?;
    if candidates.is_empty() {
        return Ok(result);
    }
    let device = device_id(&fs::metadata(home)?);
    let staging_path = crate::qemu_engine::staging_path(home);
    for path in candidates {
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                result.failures.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let valid_type = if path == staging_path {
            meta.is_dir()
        } else {
            meta.is_file()
        };
        if device_id(&meta) != device || !valid_type {
            result.failures.push(format!(
                "{}: unexpected file type or volume",
                path.display()
            ));
            continue;
        }
        let usage = candidate_usage(std::slice::from_ref(&path), StorageKind::Downloads);
        if !usage.complete {
            result
                .failures
                .push(format!("{}: could not inspect every entry", path.display()));
            continue;
        }
        match crate::util::remove_path(&path) {
            Ok(()) => result.removed_bytes = result.removed_bytes.saturating_add(usage.bytes),
            Err(error) => result.failures.push(format!("{}: {error}", path.display())),
        }
    }
    Ok(result)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::symlink;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "cabin-storage-{name}-{}-{}",
                std::process::id(),
                crate::util::now_millis()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn inventory_counts_sparse_blocks_and_links_without_following_symlinks() {
        let scratch = Scratch::new("allocation");
        let dir = scratch.0.join("vm");
        let disk = StatePaths::new(&dir).instance(NAME).disk();
        fs::create_dir_all(disk.parent().unwrap()).unwrap();
        let mut file = fs::File::create(&disk).unwrap();
        file.write_all(&[42; 4096]).unwrap();
        file.set_len(2 * 1024 * 1024 * 1024).unwrap();
        let allocated = allocated_bytes(&file.metadata().unwrap());
        assert!(allocated > 0 && allocated < file.metadata().unwrap().len());
        let outside = scratch.0.join("shared.txt");
        fs::write(&outside, vec![255; 4 * 1024 * 1024]).unwrap();
        symlink(&outside, dir.join("external")).unwrap();
        let mut scan = Inventory::default();
        scan.measure_vm(&dir);
        let rows = scan.finish(&dir);
        assert_eq!(
            rows.iter()
                .find(|entry| entry.kind == StorageKind::VirtualDisk)
                .unwrap()
                .bytes,
            allocated
        );
        assert!(
            rows.iter().map(|entry| entry.bytes).sum::<u64>()
                < allocated_bytes(&fs::metadata(&outside).unwrap())
        );
        let link = scratch.0.join("disk-link");
        fs::hard_link(&disk, &link).unwrap();
        let mut scan = Inventory::default();
        scan.measure(&disk, StorageKind::VirtualDisk, None);
        scan.measure(&link, StorageKind::GuestFiles, None);
        let total = scan
            .finish(&dir)
            .iter()
            .map(|entry| entry.bytes)
            .sum::<u64>();
        assert_eq!(total, allocated);
    }

    #[test]
    fn cleanup_preserves_live_machine_data_and_rechecks_known_candidates() {
        let scratch = Scratch::new("cleanup");
        let home = scratch.0.join("vm");
        let paths = StatePaths::new(&home);
        let instance = paths.instance(NAME);
        let logs = paths.instance_logs(NAME);
        let staging = crate::qemu_engine::staging_path(&home);
        for dir in [
            paths.images(),
            logs.clone(),
            instance.root().to_path_buf(),
            staging.clone(),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        let arch = crate::arch::GuestArch::current().debian();
        let image = paths.image(&format!("ubuntu-26.04-minimal-cloudimg-{arch}.img"));
        let partial = crate::download::partial_path(&image);
        let recent = logs.join("start-recent.log");
        let old = logs.join("start-old.log");
        let unknown = paths.image("unrelated.img.part");
        let archive = crate::qemu_engine::archive_path(&home);
        let durable = [image, instance.disk(), recent, unknown, archive.clone()];
        for path in durable
            .iter()
            .chain([partial.clone(), old.clone(), staging.join("unpacked")].iter())
        {
            fs::write(path, vec![42; 4096]).unwrap();
        }
        fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(
                SystemTime::now() - Duration::from_secs((LOG_RETENTION_DAYS + 1) * 86_400),
            ))
            .unwrap();
        let machine = crate::machine::Machine::new(&home, env!("CARGO_MANIFEST_DIR"));
        fs::write(instance.pidfile(), std::process::id().to_string()).unwrap();
        assert!(machine.clean_storage(CleanupTarget::Downloads).is_err());
        assert!(partial.exists());
        fs::remove_file(instance.pidfile()).unwrap();
        let lock = crate::lock::FileLock::acquire(&paths.operation_lock()).unwrap();
        assert!(machine.clean_storage(CleanupTarget::Logs).is_err());
        drop(lock);
        assert!(
            machine
                .clean_storage(CleanupTarget::Downloads)
                .unwrap()
                .removed_bytes
                > 0
        );
        assert!(
            machine
                .clean_storage(CleanupTarget::Logs)
                .unwrap()
                .removed_bytes
                > 0
        );
        assert!(!old.exists() && !partial.exists() && !staging.exists());
        assert!(durable.iter().all(|path| path.is_file()));
        assert_eq!(
            machine
                .clean_storage(CleanupTarget::Downloads)
                .unwrap()
                .removed_bytes,
            0
        );
        assert_eq!(
            machine
                .clean_storage(CleanupTarget::Logs)
                .unwrap()
                .removed_bytes,
            0
        );
        if cfg!(target_os = "macos") {
            let engine = crate::qemu_engine::root(&home);
            for name in crate::arch::GuestArch::current().engine_files() {
                let path = engine.join(name);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, [42]).unwrap();
            }
            assert!(
                machine
                    .clean_storage(CleanupTarget::Downloads)
                    .unwrap()
                    .removed_bytes
                    > 0
            );
            assert!(!archive.exists());
            assert!(crate::qemu_engine::installed(&home).is_some());
        }
    }

    #[test]
    fn cleanup_refuses_redirected_cache_and_log_directories() {
        let scratch = Scratch::new("symlinks");
        let home = scratch.0.join("vm");
        let paths = StatePaths::new(&home);
        fs::create_dir(&home).unwrap();
        let shared = scratch.0.join("shared");
        fs::create_dir(&shared).unwrap();
        let partial = shared.join(format!(
            "ubuntu-26.04-minimal-cloudimg-{}.img.part",
            crate::arch::GuestArch::current().debian()
        ));
        fs::write(&partial, "keep").unwrap();
        symlink(&shared, paths.images()).unwrap();
        symlink(&shared, paths.logs()).unwrap();
        let machine = crate::machine::Machine::new(&home, env!("CARGO_MANIFEST_DIR"));
        assert!(machine.clean_storage(CleanupTarget::Downloads).is_err());
        assert!(machine.clean_storage(CleanupTarget::Logs).is_err());
        assert_eq!(fs::read_to_string(partial).unwrap(), "keep");
    }
}
