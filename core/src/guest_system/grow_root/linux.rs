use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

use super::*;
use crate::system::HostSystem;

pub(in crate::guest_system) fn run(target_bytes: u64) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(Error::Unavailable(
            "grow-root must run through the root guest helper".into(),
        ));
    }
    let lock_path = Path::new("/run/cabin-grow-root.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(lock_path)
        .map_err(|error| Error::path("open", lock_path, error))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0 {
        return Err(Error::io(
            "lock guest root growth",
            std::io::Error::last_os_error(),
        ));
    }

    let cloud_init = Path::new("/usr/bin/cloud-init").try_exists()?;
    GrowRoot {
        system: Arc::new(HostSystem {
            path: "/usr/sbin:/usr/bin:/sbin:/bin".into(),
        }),
        probe: Box::new(discover),
        target_bytes,
        cloud_init,
        prepared: None,
    }
    .run()
}

fn discover(system: &dyn System) -> Result<RootDevice> {
    let mount = system.check(
        &spec([
            "findmnt",
            "--kernel",
            "--raw",
            "--noheadings",
            "--output",
            "MAJ:MIN,FSTYPE,FSROOT,OPTIONS",
            "--target",
            "/",
        ])
        .timeout(QUERY_TIMEOUT),
    )?;
    let unsupported = || {
        Error::invalid(
            "Root must be a writable ext4 filesystem mounted directly on a physical disk partition",
        )
    };
    let fields: Vec<_> = mount.split_whitespace().collect();
    let [dev, "ext4", "/", options] = fields.as_slice() else {
        return Err(unsupported());
    };
    if !options.split(',').any(|option| option == "rw") || device_number(dev).is_none() {
        return Err(unsupported());
    }
    let path = Path::new("/sys/dev/block").join(dev);
    let partition_sys =
        fs::canonicalize(&path).map_err(|error| Error::path("resolve", &path, error))?;
    let disk_sys = partition_sys.parent().ok_or_else(unsupported)?;
    if !disk_sys.starts_with("/sys/devices") || disk_sys.starts_with("/sys/devices/virtual") {
        return Err(unsupported());
    }
    let number = sys_number(&partition_sys, "partition")?;
    if number == 0
        || sys_number(disk_sys, "queue/logical_block_size")? != KERNEL_SECTOR
        || sys_number(disk_sys, "ro")? != 0
        || sys_number(&partition_sys, "ro")? != 0
    {
        return Err(unsupported());
    }
    let partition = device_node(&partition_sys)?;
    let disk = device_node(disk_sys)?;
    if read(&partition_sys.join("dev"))?.trim() != *dev {
        return Err(unsupported());
    }
    check_gpt_headers(&disk)?;
    // A low-level probe also rejects ambiguous signatures, including stale LVM or RAID metadata.
    let signature = system.check(
        &spec([
            "blkid",
            "--probe",
            "--match-tag",
            "TYPE",
            "--output",
            "value",
            &partition,
        ])
        .timeout(QUERY_TIMEOUT),
    )?;
    if signature.trim() != "ext4" {
        return Err(unsupported());
    }
    Ok(RootDevice {
        disk,
        partition,
        number,
        disk_sectors: sys_number(disk_sys, "size")?,
        kernel_start: sys_number(&partition_sys, "start")?,
        kernel_size: sys_number(&partition_sys, "size")?,
    })
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|error| Error::path("read", path, error))
}

fn sys_number(device: &Path, field: &str) -> Result<u64> {
    let path = device.join(field);
    number(read(&path)?.trim())
        .ok_or_else(|| Error::invalid(format!("Invalid block device field {}", path.display())))
}

fn device_number(text: &str) -> Option<(u64, u64)> {
    let (major, minor) = text.split_once(':')?;
    Some((number(major)?, number(minor)?))
}

fn device_node(sys: &Path) -> Result<String> {
    let invalid = || Error::invalid("The guest's /dev and sysfs block devices disagree");
    let name = sys
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(invalid)?;
    let path = Path::new("/dev").join(name);
    let expected = device_number(read(&sys.join("dev"))?.trim()).ok_or_else(invalid)?;
    let metadata =
        fs::symlink_metadata(&path).map_err(|error| Error::path("inspect", &path, error))?;
    let id = metadata.rdev() as libc::dev_t;
    let actual = (u64::from(libc::major(id)), u64::from(libc::minor(id)));
    if !metadata.file_type().is_block_device() || expected != actual {
        return Err(invalid());
    }
    Ok(format!("/dev/{name}"))
}

fn check_gpt_headers(disk: &str) -> Result<()> {
    // A hybrid MBR or a nonstandard GPT entry array could be altered by rewriting
    // the GPT with growpart. libfdisk separately validates the GPT headers and entries.
    let mut headers = [0; 1024];
    File::open(disk)
        .and_then(|mut file| file.read_exact(&mut headers))
        .map_err(|error| Error::io(format!("read GPT headers on {disk}"), error))?;
    let entries = &headers[446..510];
    let mut protective = entries
        .as_chunks::<16>()
        .0
        .iter()
        .filter(|entry| entry.iter().any(|byte| *byte != 0));
    if headers[510..512] != [0x55, 0xaa]
        || !protective
            .next()
            .is_some_and(|entry| entry[0] == 0 && entry[4] == 0xee && entry[8..12] == [1, 0, 0, 0])
        || protective.next().is_some()
        || &headers[512..520] != b"EFI PART"
        || headers[592..600] != [128, 0, 0, 0, 128, 0, 0, 0]
    {
        return Err(Error::invalid(
            "Root disk must use a standard GPT and protective MBR",
        ));
    }
    Ok(())
}
