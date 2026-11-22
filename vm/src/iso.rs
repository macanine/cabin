//! Minimal ISO 9660 writer for the cloud-init CIDATA seed.
//!
//! macOS and Linux build the seed ISO with `hdiutil`/`xorriso`. Windows hosts
//! do not ship an ISO tool, so this writer produces the image directly: an
//! ISO 9660 volume labelled `CIDATA` with a supplementary Joliet volume for
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

    fn file(name: &str, data_len: usize, extent: u32) -> Self {
        let mut identifier = name.as_bytes().to_vec();
        identifier.extend_from_slice(b";1");
        Self {
            name: identifier,
            extent,
            size: data_len as u32,
            directory: false,
        }
    }

    fn joliet_file(name: &str, data_len: usize, extent: u32) -> Self {
        Self {
            name: name.bytes().flat_map(|byte| [0u8, byte]).collect(),
            extent,
            size: data_len as u32,
            directory: false,
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

fn volume_stamp() -> [u8; 17] {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };
    let mut stamp = [0u8; 17];
    let text = format!("{year:04}{month:02}{day:02}");
    for (index, byte) in text.bytes().enumerate() {
        stamp[index] = byte - b'0';
    }
    stamp[12] = (rest / 3600) as u8;
    stamp[13] = ((rest % 3600) / 60) as u8;
    stamp[14] = (rest % 60) as u8;
    stamp[16] = 0; // GMT offset unknown
    stamp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An independent ISO 9660 reader used to verify the written structure.
    struct Iso {
        blocks: Vec<Vec<u8>>,
    }

    impl Iso {
        fn parse(bytes: &[u8]) -> Self {
            Self {
                blocks: bytes.chunks(BLOCK).map(<[u8]>::to_vec).collect(),
            }
        }

        fn le32(bytes: &[u8], at: usize) -> u32 {
            u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
        }

        fn volume_id(&self, descriptor: usize) -> String {
            let block = &self.blocks[descriptor];
            if block[40] == 0 {
                // Joliet: UCS-2BE, NUL-padded to 32 bytes.
                let decoded: String = block[40..72]
                    .chunks(2)
                    .map(|pair| {
                        char::from_u32(u16::from_be_bytes([pair[0], pair[1]]).into())
                            .unwrap_or('?')
                    })
                    .collect();
                decoded.trim_end_matches('\0').to_string()
            } else {
                String::from_utf8_lossy(&block[40..72]).trim_end().to_string()
            }
        }

        fn root_extent(&self, descriptor: usize) -> u32 {
            Self::le32(&self.blocks[descriptor], 156 + 2)
        }

        /// Directory records of an extent: (name, extent, size).
        fn directory(&self, extent: u32) -> Vec<(String, u32, u32)> {
            let block = &self.blocks[extent as usize];
            let mut result = Vec::new();
            let mut at = 0;
            while at < BLOCK && block[at] > 0 {
                let length = block[at] as usize;
                let name_len = block[at + 32] as usize;
                let raw = &block[at + 33..at + 33 + name_len];
                let name = if name_len == 1 && raw[0] == 0 {
                    ".".to_string()
                } else if name_len == 1 && raw[0] == 1 {
                    "..".to_string()
                } else if raw.len() % 2 == 0 && raw.first() == Some(&0) {
                    // Joliet UCS-2BE names start with a zero byte.
                    raw.chunks(2)
                        .map(|pair| char::from_u32(u16::from_be_bytes([pair[0], pair[1]]).into()).unwrap_or('?'))
                        .collect()
                } else {
                    String::from_utf8_lossy(raw)
                        .trim_end_matches(";1")
                        .to_string()
                };
                result.push((
                    name,
                    Self::le32(block, at + 2),
                    Self::le32(block, at + 10),
                ));
                at += length;
            }
            result
        }

        fn file_data(&self, extent: u32, size: u32) -> Vec<u8> {
            let start = extent as usize;
            let spanned = size.div_ceil(BLOCK as u32) as usize;
            self.blocks[start..start + spanned].concat()[..size as usize].to_vec()
        }
    }

    fn sample_files() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("user-data", b"#cloud-config\nusers: []\n".to_vec()),
            ("meta-data", b"instance-id: cabin\n".to_vec()),
            ("network-config", vec![0x61; 5000]), // multi-block file
        ]
    }

    #[test]
    fn writes_cidata_structure_with_names_and_content() {
        let dir = std::env::temp_dir().join(format!(
            "cabin-iso-{}-{}",
            std::process::id(),
            crate::util::now_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("seed.iso");
        let files = sample_files();
        let staged: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(name, data)| (*name, data.as_slice()))
            .collect();
        build_iso(&output, &staged).unwrap();

        let bytes = std::fs::read(&output).unwrap();
        assert_eq!(bytes.len() % BLOCK, 0);
        let iso = Iso::parse(&bytes);
        assert_eq!(iso.volume_id(16), "CIDATA");
        assert_eq!(iso.volume_id(17), "CIDATA");

        let root = iso.directory(iso.root_extent(16));
        for (name, expected) in [
            ("user-data", files[0].1.as_slice()),
            ("meta-data", files[1].1.as_slice()),
            ("network-config", files[2].1.as_slice()),
        ] {
            let record = root
                .iter()
                .find(|(found, _, _)| found == name)
                .unwrap_or_else(|| panic!("missing {name} in {:?}", root));
            assert_eq!(record.2 as usize, expected.len(), "{name} size");
            assert_eq!(iso.file_data(record.1, record.2), expected, "{name} data");
        }

        let joliet = iso.directory(iso.root_extent(17));
        assert!(joliet.iter().any(|(name, _, _)| name == "network-config"));
        // The "." record must cover the whole directory extent.
        assert_eq!(root[0].2 as usize, 2048);

        // File extents must not overlap.
        let extents: Vec<(u32, u32)> = root
            .iter()
            .filter(|(name, _, _)| !matches!(name.as_str(), "." | ".."))
            .map(|(_, extent, size)| (*extent, size.div_ceil(BLOCK as u32)))
            .collect();
        for pair in extents.windows(2) {
            assert!(pair[0].0 + pair[0].1 <= pair[1].0, "overlapping extents");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_bad_names_and_empty_sets() {
        let dir = std::env::temp_dir().join(format!(
            "cabin-iso-bad-{}-{}",
            std::process::id(),
            crate::util::now_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("seed.iso");
        assert!(build_iso(&output, &[]).is_err());
        assert!(build_iso(&output, &[("with/slash", b"x")]).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
