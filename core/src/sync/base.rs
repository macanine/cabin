//! Compact on-disk form of the sync base.
//!
//! The base stores the last synced stamps in a versioned binary format.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use super::plan::{BaseRecord, Stamp};
use super::scan::{EntryKind, read_digest, write_digest};

const MAGIC: &[u8; 8] = b"VASYNCB2";
const LEGACY_MAGIC: &[u8; 8] = b"VASYNCB1";
const MAX_PATH: u32 = 1 << 20;
const MAX_RECORDS: u64 = 10_000_000;

/// Read a compact base, or `None` when the file is absent.
pub fn read(path: &Path) -> io::Result<Option<BTreeMap<String, BaseRecord>>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut input = BufReader::with_capacity(1 << 20, file);
    let mut magic = [0; 8];
    input.read_exact(&mut magic)?;
    let legacy = &magic == LEGACY_MAGIC;
    if !legacy && &magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid sync base header",
        ));
    }
    let count = read_u64(&mut input)?;
    if count > MAX_RECORDS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sync base has too many records",
        ));
    }
    let mut base = BTreeMap::new();
    for _ in 0..count {
        let path = read_string(&mut input)?;
        if super::safe_relative(&path).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid sync base path",
            ));
        }
        let mac = read_stamp(&mut input, legacy)?;
        let vm = read_stamp(&mut input, legacy)?;
        if base.insert(path, BaseRecord { mac, vm }).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "duplicate sync base path",
            ));
        }
    }
    if input.read(&mut [0])? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing data in sync base",
        ));
    }
    Ok(Some(base))
}

/// Write a compact base atomically.
pub fn write(path: &Path, base: &BTreeMap<String, BaseRecord>) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut out = BufWriter::with_capacity(1 << 20, tmp.as_file_mut());
        out.write_all(MAGIC)?;
        write_u64(&mut out, base.len() as u64)?;
        for (path, record) in base {
            write_string(&mut out, path)?;
            write_stamp(&mut out, &record.mac)?;
            write_stamp(&mut out, &record.vm)?;
        }
        out.flush()?;
    }
    tmp.persist(path).map(|_| ()).map_err(|error| error.error)
}

fn read_stamp(input: &mut impl Read, legacy: bool) -> io::Result<Stamp> {
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
    let size = read_u64(input)?;
    let mtime_ns = read_u64(input)? as i64;
    let target = read_optional_string(input)?;
    let digest = if !legacy && kind == EntryKind::File {
        read_digest(input)?
    } else {
        None
    };
    Ok(Stamp {
        kind,
        size,
        mtime_ns,
        target,
        digest,
    })
}

fn write_stamp(out: &mut impl Write, stamp: &Stamp) -> io::Result<()> {
    let kind = match stamp.kind {
        EntryKind::File => 0,
        EntryKind::Dir => 1,
        EntryKind::Symlink => 2,
    };
    out.write_all(&[kind])?;
    write_u64(out, stamp.size)?;
    write_u64(out, stamp.mtime_ns as u64)?;
    write_optional_string(out, stamp.target.as_deref())?;
    if stamp.kind == EntryKind::File {
        write_digest(out, stamp.digest.as_ref())?;
    }
    Ok(())
}

fn read_string(input: &mut impl Read) -> io::Result<String> {
    let len = read_u32(input)?;
    if len > MAX_PATH {
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

    fn stamp(size: u64, target: Option<&str>) -> Stamp {
        Stamp {
            kind: if target.is_some() {
                EntryKind::Symlink
            } else {
                EntryKind::File
            },
            size,
            mtime_ns: 1_700_000_000_123_456_789,
            target: target.map(str::to_owned),
            digest: target.is_none().then_some([42; 32]),
        }
    }

    #[test]
    fn reads_legacy_bases_and_preserves_unknown_records_on_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("base.bin");
        let mut bytes = LEGACY_MAGIC.to_vec();
        write_u64(&mut bytes, 1).unwrap();
        write_string(&mut bytes, "file").unwrap();
        for mtime in [42, 43] {
            bytes.push(0);
            write_u64(&mut bytes, 5).unwrap();
            write_u64(&mut bytes, mtime).unwrap();
            write_optional_string(&mut bytes, None).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        let base = read(&path).unwrap().unwrap();
        assert_eq!(base["file"].mac.digest, None);
        assert_eq!(base["file"].vm.digest, None);
        assert_eq!(base["file"].mac.mtime_ns, 42);
        assert_eq!(base["file"].vm.mtime_ns, 43);
        write(&path, &base).unwrap();
        assert!(fs::read(&path).unwrap().starts_with(MAGIC));
        assert_eq!(read(&path).unwrap().unwrap(), base);
    }

    #[test]
    fn round_trips_paths_stamps_and_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("base.bin");
        assert!(read(&path).unwrap().is_none());
        let mut base = BTreeMap::new();
        base.insert(
            "src/main.rs".into(),
            BaseRecord {
                mac: stamp(12, None),
                vm: stamp(12, None),
            },
        );
        base.insert(
            "link".into(),
            BaseRecord {
                mac: stamp(0, Some("src/main.rs")),
                vm: stamp(0, Some("src/main.rs")),
            },
        );
        write(&path, &base).unwrap();
        assert_eq!(read(&path).unwrap().unwrap(), base);
    }

    #[test]
    fn malformed_files_do_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("base.bin");
        fs::write(&path, b"not-a-base").unwrap();
        assert_eq!(read(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
        fs::write(&path, b"short").unwrap();
        assert_eq!(
            read(&path).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        let mut broken = MAGIC.to_vec();
        broken.extend_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&path, broken).unwrap();
        assert!(read(&path).is_err());
    }
}
