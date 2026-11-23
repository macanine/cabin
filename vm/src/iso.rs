//! Minimal ISO 9660 writer for the cloud-init CIDATA seed.
//!
//! Every host builds the seed in process, so no ISO tool (`hdiutil`,
//! `xorriso`, `oscdimg`) is a requirement. The writer produces the image
//! directly: an ISO 9660 volume labelled `CIDATA` with a supplementary Joliet volume for
//! long names, holding a flat directory of small files (`user-data`,
//! `meta-data`, `network-config`). The layout is intentionally conservative —
//! classic (little, big) byte pairs, plain contiguous extents, one directory
//! level — so udev's label probe, the Linux isofs driver and cloud-init read
//! it without surprises.

use std::path::Path;

use crate::Result;
use crate::util::message;

/// Logical block size used throughout the image.
const BLOCK: usize = 2048;
const LABEL: &[u8; 6] = b"CIDATA";

// Fixed volume layout: descriptors, one path-table pair per volume, the two
// root directories, then one contiguous extent per file.
const PVD_EXTENT: u32 = 16;
const SVD_EXTENT: u32 = 17;
const TERMINATOR_EXTENT: u32 = 18;
const L_PATH_EXTENT: u32 = 19;
const M_PATH_EXTENT: u32 = 20;
const JOLIET_L_PATH_EXTENT: u32 = 21;
const JOLIET_M_PATH_EXTENT: u32 = 22;
const ROOT_EXTENT: u32 = 23;
const JOLIET_ROOT_EXTENT: u32 = 24;
const FIRST_FILE_EXTENT: u32 = 25;

/// (little-endian u32, big-endian u32) dual byte order field.
fn both32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
    out[at + 4..at + 8].copy_from_slice(&value.to_be_bytes());
}

fn both16(out: &mut [u8], at: usize, value: u16) {
    out[at..at + 2].copy_from_slice(&value.to_le_bytes());
    out[at + 2..at + 4].copy_from_slice(&value.to_be_bytes());
}

fn pad_to_block(out: &mut Vec<u8>) {
    let remainder = out.len() % BLOCK;
    if remainder != 0 {
        out.extend(std::iter::repeat_n(0u8, BLOCK - remainder));
    }
}

fn blocks_for(size: usize) -> usize {
    size.div_ceil(BLOCK).max(1)
}

/// One planned directory record: 33 header bytes + identifier (+ pad byte).
#[derive(Clone)]
struct Record {
    /// Identifier: "." -> [0], ".." -> [1], files -> name + ";1", Joliet ->
    /// UCS-2BE name without a version suffix.
    name: Vec<u8>,
    extent: u32,
    size: u32,
    directory: bool,
}

impl Record {
    fn dot(extent: u32, size: u32) -> Self {
        Self {
            name: vec![0],
            extent,
            size,
            directory: true,
        }
    }

    fn dot_dot(extent: u32, size: u32) -> Self {
        Self {
            name: vec![1],
            extent,
            size,
            directory: true,
        }
    }

    fn record_size(&self) -> usize {
        let mut length = 33 + self.name.len();
        if self.name.len() % 2 == 0 {
            length += 1; // pad byte keeps records even-length
        }
        length
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.push(self.record_size() as u8); // 0: record length
        out.push(0); // 1: extended attribute length
        out.extend_from_slice(&self.extent.to_le_bytes()); // 2-9
        out.extend_from_slice(&self.extent.to_be_bytes());
        out.extend_from_slice(&self.size.to_le_bytes()); // 10-17: data length
        out.extend_from_slice(&self.size.to_be_bytes());
        // 18-24: recording date/time (zeros: unknown, accepted by readers).
        out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0]);
        out.push(if self.directory { 0b10 } else { 0 }); // 25: flags
        out.push(0); // 26: file unit size
        out.push(0); // 27: interleave gap
        out.extend_from_slice(&1u16.to_le_bytes()); // 28-31: volume sequence
        out.extend_from_slice(&1u16.to_be_bytes());
        out.push(self.name.len().max(1) as u8); // 32: identifier length
        if self.name.is_empty() {
            out.push(0);
        } else {
            out.extend_from_slice(&self.name);
        }
        if self.name.len() % 2 == 0 {
            out.push(0); // pad byte
        }
    }
}

/// Serialize a directory (records, then zero padding to the sector).
fn directory_bytes(records: &[Record]) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        record.write(&mut out);
    }
    pad_to_block(&mut out);
    out
}

/// Plan "." / ".." / file records for one root directory. `name` builds the
/// identifier for a staged file; the "." / ".." sizes are patched after the
/// first serialization fixes the directory length.
fn plan_root<Name>(
    files: &[(String, Vec<u8>)],
    extents: &[u32],
    self_extent: u32,
    name: Name,
) -> (Vec<u8>, Record)
where
    Name: Fn(&str) -> Vec<u8>,
{
    let mut records = vec![Record::dot(self_extent, 0), Record::dot_dot(self_extent, 0)];
    for (index, (file_name, data)) in files.iter().enumerate() {
        records.push(Record {
            name: name(file_name),
            extent: extents[index],
            size: data.len() as u32,
            directory: false,
        });
    }
    let mut directory = directory_bytes(&records);
    let length = directory.len() as u32;
    records[0].size = length;
    records[1].size = length;
    directory = directory_bytes(&records);
    (directory, records[0].clone())
}

fn path_table_bytes(extent: u32, big_endian: bool) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(1); // identifier length: the root carries a single "\0"
    out.push(0); // extended attribute record length
    if big_endian {
        out.extend_from_slice(&extent.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // parent: itself
    } else {
        out.extend_from_slice(&extent.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
    }
    out.push(0); // identifier "\0"
    if out.len() % 2 != 0 {
        out.push(0);
    }
    pad_to_block(&mut out);
    out
}

/// Build the CIDATA ISO with the staged files (flat, in given order) and
/// write it to `output`.
pub fn build_iso(output: &Path, files: &[(&str, &[u8])]) -> Result<()> {
    let mut staged: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(name, data)| ((*name).to_string(), data.to_vec()))
        .collect();
    staged.sort_by(|a, b| a.0.cmp(&b.0));
    staged.dedup_by(|a, b| a.0 == b.0);
    if staged.is_empty() {
        return message("the seed ISO needs at least one file");
    }
    for (name, _) in &staged {
        if name.is_empty()
            || name.len() > 120
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != b'/')
        {
            return message(format!("invalid seed file name: {name:?}"));
        }
    }

    // Assign file extents; every other offset in the image is fixed.
    let mut next_extent = FIRST_FILE_EXTENT;
    let extents: Vec<u32> = staged
        .iter()
        .map(|(_, data)| {
            let extent = next_extent;
            next_extent += blocks_for(data.len()) as u32;
            extent
        })
        .collect();
    let total_blocks = next_extent;

    let (iso_directory, iso_root) = plan_root(&staged, &extents, ROOT_EXTENT, |name| {
        let mut identifier = name.as_bytes().to_vec();
        identifier.extend_from_slice(b";1");
        identifier
    });
    let (joliet_directory, joliet_root) = plan_root(&staged, &extents, JOLIET_ROOT_EXTENT, |name| {
        name.bytes().flat_map(|byte| [0u8, byte]).collect()
    });

    let path_table = path_table_bytes(0, false);
    let path_table_size = path_table.len() as u32;

    let mut image: Vec<u8> = Vec::with_capacity(total_blocks as usize * BLOCK);
    image.resize(PVD_EXTENT as usize * BLOCK, 0);
    image.resize((PVD_EXTENT as usize + 1) * BLOCK, 0);
    put_volume_descriptor(
        &mut image[PVD_EXTENT as usize * BLOCK..(PVD_EXTENT as usize + 1) * BLOCK],
        true,
        total_blocks,
        path_table_size,
        &iso_root,
    );
    image.resize((SVD_EXTENT as usize + 1) * BLOCK, 0);
    put_volume_descriptor(
        &mut image[SVD_EXTENT as usize * BLOCK..SVD_EXTENT as usize * BLOCK + BLOCK],
        false,
        total_blocks,
        path_table_size,
        &joliet_root,
    );
    put_terminator(&mut image);

    image.resize(L_PATH_EXTENT as usize * BLOCK, 0);
    image.extend_from_slice(&path_table_bytes(ROOT_EXTENT, false));
    image.resize(M_PATH_EXTENT as usize * BLOCK, 0);
    image.extend_from_slice(&path_table_bytes(ROOT_EXTENT, true));
    image.resize(JOLIET_L_PATH_EXTENT as usize * BLOCK, 0);
    image.extend_from_slice(&path_table_bytes(JOLIET_ROOT_EXTENT, false));
    image.resize(JOLIET_M_PATH_EXTENT as usize * BLOCK, 0);
    image.extend_from_slice(&path_table_bytes(JOLIET_ROOT_EXTENT, true));

    image.resize(ROOT_EXTENT as usize * BLOCK, 0);
    image.extend_from_slice(&iso_directory);
    image.resize(JOLIET_ROOT_EXTENT as usize * BLOCK, 0);
    image.extend_from_slice(&joliet_directory);

    for (index, (_, data)) in staged.iter().enumerate() {
        image.resize(extents[index] as usize * BLOCK, 0);
        image.extend_from_slice(data);
        pad_to_block(&mut image);
    }

    if image.len() != total_blocks as usize * BLOCK {
        return message("seed ISO layout mismatch");
    }
    std::fs::write(output, &image)?;
    Ok(())
}

/// Primary (iso=true) or Joliet supplementary (iso=false) volume descriptor.
fn put_volume_descriptor(
    out: &mut [u8],
    iso: bool,
    total_blocks: u32,
    path_table_size: u32,
    root: &Record,
) {
    let (l_table, m_table) = if iso {
        (L_PATH_EXTENT, M_PATH_EXTENT)
    } else {
        (JOLIET_L_PATH_EXTENT, JOLIET_M_PATH_EXTENT)
    };
    out.fill(0);
    out[0] = if iso { 1 } else { 2 }; // volume descriptor type
    out[1..6].copy_from_slice(b"CD001");
    out[6] = 1; // descriptor version
    out[7..39].fill(b' '); // system identifier
    if iso {
        out[40..46].copy_from_slice(LABEL); // volume identifier
        out[46..72].fill(b' ');
        out[72..80].fill(0); // unused
    } else {
        // Volume identifier in UCS-2BE (Joliet).
        for (index, byte) in LABEL.iter().enumerate() {
            out[40 + index * 2] = 0;
            out[41 + index * 2] = *byte;
        }
        out[52..72].fill(0);
        out[72..80].fill(0); // unused
    }
    both32(out, 80, total_blocks); // volume space size
    if !iso {
        // Escape sequences replace the PVD's second unused area (88..120).
        out[88..91].copy_from_slice(b"%/@"); // UCS-2 level 3
        out[91..120].fill(0);
    }
    both16(out, 120, 1); // volume set size
    both16(out, 124, 1); // volume sequence number
    both16(out, 128, BLOCK as u16); // logical block size
    both32(out, 132, path_table_size); // path table size
    out[140..144].copy_from_slice(&l_table.to_le_bytes());
    out[148..152].copy_from_slice(&m_table.to_be_bytes());
    let mut root_record = Vec::new();
    root.write(&mut root_record);
    out[156..156 + 34].copy_from_slice(&root_record[..34]);
    // Identifiers are d-characters: space-padded strings nobody validates.
    out[190..222].fill(b' '); // volume set identifier
    out[222..350].fill(b' '); // publisher
    out[350..478].fill(b' '); // data preparer
    out[478..515].fill(b' '); // application identifier
    out[515..552].fill(b' '); // copyright file identifier
    out[552..589].fill(b' '); // abstract file identifier
    out[589..626].fill(b' '); // bibliographic file identifier
    let stamp = volume_stamp();
    out[626..643].copy_from_slice(&stamp); // creation date/time
    out[643..660].copy_from_slice(&stamp); // modification date/time
    out[694] = 1; // file structure version
}

fn put_terminator(image: &mut Vec<u8>) {
    image.resize((TERMINATOR_EXTENT as usize + 1) * BLOCK, 0);
    let base = TERMINATOR_EXTENT as usize * BLOCK;
    let out = &mut image[base..base + BLOCK];
    out.fill(0);
    out[0] = 255;
    out[1..6].copy_from_slice(b"CD001");
    out[6] = 1;
}

/// Volume dates: 16 ASCII digits (`YYYYMMDDHHMMSScc`) and a GMT offset byte.
fn volume_stamp() -> [u8; 17] {
    let text = chrono::Utc::now().format("%Y%m%d%H%M%S00").to_string();
    let mut stamp = [0u8; 17]; // offset 0: GMT
    stamp[..16].copy_from_slice(text.as_bytes());
    stamp
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

type Volume = (String, Vec<(String, Vec<u8>)>);

/// Read a volume's label and root-directory files, ignoring "." and "..".
/// `joliet` selects the supplementary (UCS-2) tree over the primary one.
fn read_volume(image: &[u8], descriptor: u32, joliet: bool) -> Option<Volume> {
    let blocks = |extent: u32, length: usize| {
        let start = extent as usize * BLOCK;
        image.get(start..start.checked_add(length)?)
    };
    let volume = blocks(descriptor, BLOCK)?;
    if &volume[1..6] != b"CD001" || volume[0] != if joliet { 2 } else { 1 } {
        return None;
    }
    let text = |raw: &[u8]| -> String {
        if joliet {
            let units: Vec<u16> =
                raw.chunks(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect();
            String::from_utf16_lossy(&units)
        } else {
            String::from_utf8_lossy(raw).into_owned()
        }
    };
    let label = text(&volume[40..72]).trim_end_matches(['\0', ' ']).to_string();
    let directory = blocks(le32(volume, 158)?, le32(volume, 166)? as usize)?;
    let mut files = Vec::new();
    let mut at = 0;
    while at < directory.len() && directory[at] > 0 {
        let record = directory.get(at..at + directory[at] as usize)?;
        let raw = record.get(33..33 + *record.get(32)? as usize)?;
        if !matches!(raw, [0] | [1]) && record.get(25)? & 0b10 == 0 {
            let data = blocks(le32(record, 2)?, le32(record, 10)? as usize)?;
            files.push((text(raw).trim_end_matches(";1").to_string(), data.to_vec()));
        }
        at += record.len();
    }
    Some((label, files))
}

/// Read back an image written by [`build_iso`]: its label and files. The
/// primary and Joliet trees must agree.
pub fn read_iso(path: &Path) -> Result<Volume> {
    let image = std::fs::read(path)?;
    match (read_volume(&image, PVD_EXTENT, false), read_volume(&image, SVD_EXTENT, true)) {
        (Some(primary), Some(joliet)) if primary == joliet => Ok(primary),
        _ => message("the seed ISO is unreadable or its volumes disagree"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn sample() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("user-data", b"#cloud-config\nusers: []\n".to_vec()),
            ("meta-data", b"instance-id: cabin\n".to_vec()),
            ("network-config", vec![0x61; 5000]), // multi-block file
        ]
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cabin-iso-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn build(path: &Path) {
        let files = sample();
        let staged: Vec<(&str, &[u8])> =
            files.iter().map(|(name, data)| (*name, data.as_slice())).collect();
        build_iso(path, &staged).unwrap();
    }

    #[test]
    fn round_trips_the_label_names_and_content() {
        let dir = scratch("seed round trip 世界");
        let output = dir.join("seed.iso");
        build(&output);
        assert_eq!(std::fs::metadata(&output).unwrap().len() % BLOCK as u64, 0);
        let (label, mut files) = read_iso(&output).unwrap();
        assert_eq!(label, "CIDATA");
        files.sort();
        let mut expected: Vec<_> =
            sample().into_iter().map(|(name, data)| (name.to_string(), data)).collect();
        expected.sort();
        assert_eq!(files, expected);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_bad_names_and_empty_sets() {
        let dir = scratch("bad");
        let output = dir.join("seed.iso");
        assert!(build_iso(&output, &[]).is_err());
        assert!(build_iso(&output, &[("with/slash", b"x")]).is_err());
        std::fs::write(&output, b"not an iso").unwrap();
        assert!(read_iso(&output).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A tool that did not write the image agrees with it. Runs where one is
    /// installed: `xorriso` (Linux, Windows with MSYS) or `hdiutil` (macOS).
    #[test]
    fn an_independent_tool_reads_the_image() {
        let dir = scratch("independent");
        let output = dir.join("seed.iso");
        build(&output);
        if let Some(xorriso) = crate::util::which("xorriso") {
            let extracted = dir.join("out");
            let info = Command::new(&xorriso).arg("-indev").arg(&output).arg("-pvd_info").output().unwrap();
            assert!(String::from_utf8_lossy(&info.stdout).to_lowercase().contains("volume id    : cidata"));
            let status = Command::new(&xorriso)
                .args(["-osirrox", "on", "-indev"])
                .arg(&output)
                .args(["-extract", "/"])
                .arg(&extracted)
                .status()
                .unwrap();
            assert!(status.success());
            assert_eq!(std::fs::read(extracted.join("network-config")).unwrap(), vec![0x61; 5000]);
        } else if cfg!(target_os = "macos") {
            let mount = dir.join("mnt");
            std::fs::create_dir_all(&mount).unwrap();
            let attach = Command::new("hdiutil")
                .args(["attach", "-readonly", "-nobrowse", "-mountpoint"])
                .arg(&mount)
                .arg(&output)
                .output()
                .unwrap();
            assert!(attach.status.success(), "{}", String::from_utf8_lossy(&attach.stderr));
            let read = std::fs::read(mount.join("network-config"));
            let _ = Command::new("hdiutil").arg("detach").arg(&mount).arg("-force").status();
            assert_eq!(read.unwrap(), vec![0x61; 5000]);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
