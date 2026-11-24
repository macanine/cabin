use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::system::fake::FakeSystem;

const DISK_SECTORS: u64 = 8 * 1024 * 1024;
const ROOT_START: u64 = 227_328;
const OLD_SIZE: u64 = 2 * 1024 * 1024;
const NEW_SIZE: u64 = DISK_SECTORS - ROOT_START - GPT_BACKUP_SECTORS - 511;
const LINUX_FS: &str = "0FC63DAF-8483-4772-8E79-3D69D8477DE4";

struct Guest {
    disk_sectors: u64,
    table_size: u64,
    kernel_size: u64,
    fs_blocks: u64,
    cloud_status: &'static str,
    cloud_code: i32,
    cloud_grows: bool,
    boot_after_root: bool,
    bad_dry_run: bool,
    fail_partx: bool,
    fail_resize: bool,
    ignore_resize: bool,
}

impl Default for Guest {
    fn default() -> Self {
        Self {
            disk_sectors: DISK_SECTORS,
            table_size: OLD_SIZE,
            kernel_size: OLD_SIZE,
            fs_blocks: OLD_SIZE / 8,
            cloud_status: "done",
            cloud_code: 0,
            cloud_grows: false,
            boot_after_root: false,
            bad_dry_run: false,
            fail_partx: false,
            fail_resize: false,
            ignore_resize: false,
        }
    }
}

impl Guest {
    fn device(&self) -> RootDevice {
        RootDevice {
            disk: "/dev/nvme0n1".into(),
            partition: "/dev/nvme0n1p1".into(),
            number: 1,
            disk_sectors: self.disk_sectors,
            kernel_start: ROOT_START,
            kernel_size: self.kernel_size,
        }
    }

    fn command(&mut self, argv: &[&str]) -> Output {
        let mut output = Output {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        };
        match argv {
            ["cloud-init", "status", "--wait", "--format", "json"] => {
                if self.cloud_grows {
                    self.table_size = NEW_SIZE;
                    self.kernel_size = NEW_SIZE;
                    self.fs_blocks = NEW_SIZE / 8;
                }
                output.stdout = json!({ "status": self.cloud_status }).to_string();
                output.code = Some(self.cloud_code);
            }
            ["sfdisk", "--json", "/dev/nvme0n1"] => {
                let lastlba = if self.table_size == OLD_SIZE && !self.boot_after_root {
                    output.stderr = "GPT PMBR size mismatch (2342900 != 8388607) will be corrected by write.\nThe backup GPT table is not on the end of the device.\n".into();
                    ROOT_START + OLD_SIZE + 2047
                } else {
                    self.disk_sectors - GPT_BACKUP_SECTORS - 1
                };
                let boot_start = if self.boot_after_root {
                    ROOT_START + self.table_size
                } else {
                    2048
                };
                output.stdout = json!({ "partitiontable": {
                    "label": "gpt", "id": "disk-id", "device": "/dev/nvme0n1", "unit": "sectors",
                    "firstlba": 34, "lastlba": lastlba, "sectorsize": 512,
                    "partitions": [
                        { "node": "/dev/nvme0n1p1", "start": ROOT_START, "size": self.table_size, "type": LINUX_FS, "uuid": "root-id" },
                        { "node": "/dev/nvme0n1p15", "start": boot_start, "size": 8192, "type": "C12A7328-F81F-11D2-BA4B-00A0C93EC93B", "uuid": "boot-id" }
                    ]
                }}).to_string();
            }
            ["dumpe2fs", "-h", "/dev/nvme0n1p1"] => {
                output.stdout = format!(
                    "Block size: 4096\nBlock count: {}\nFilesystem features: has_journal extent 64bit\nFilesystem state: clean\n",
                    self.fs_blocks
                );
            }
            [
                "growpart",
                "--fudge",
                "0",
                "--update",
                "off",
                "--dry-run",
                "/dev/nvme0n1",
                "1",
            ] => {
                output.stdout = growth_report("CHANGE:", NEW_SIZE);
                let old = dump(OLD_SIZE);
                let mut new = dump(NEW_SIZE);
                if self.bad_dry_run {
                    new = new.replace("start=2048", "start=4096");
                }
                output.stderr =
                    format!("# === old sfdisk -d ===\n{old}# === new sfdisk -d ===\n{new}");
            }
            [
                "growpart",
                "--fudge",
                "0",
                "--update",
                "off",
                "/dev/nvme0n1",
                "1",
            ] => {
                self.table_size = NEW_SIZE;
                output.stdout = growth_report("CHANGED:", NEW_SIZE);
            }
            ["partx", "--update", "--nr", "1", "/dev/nvme0n1"] => {
                if self.fail_partx {
                    output.code = Some(1);
                    output.stderr = "kernel refused the update".into();
                } else {
                    self.kernel_size = self.table_size;
                }
            }
            ["resize2fs", "/dev/nvme0n1p1", blocks] => {
                let blocks = number(blocks).unwrap();
                assert_eq!(blocks, NEW_SIZE / 8);
                assert_eq!(self.kernel_size, NEW_SIZE);
                if self.fail_resize {
                    output.code = Some(1);
                    output.stderr = "resize failed".into();
                } else if !self.ignore_resize {
                    self.fs_blocks = blocks;
                }
            }
            _ => panic!("unexpected command {argv:?}"),
        }
        output
    }
}

fn growth_report(prefix: &str, new_size: u64) -> String {
    format!(
        "{prefix} partition=1 start={ROOT_START} old: size={OLD_SIZE} end={} new: size={new_size} end={}\n",
        ROOT_START + OLD_SIZE - 1,
        ROOT_START + new_size - 1
    )
}

fn dump(root_size: u64) -> String {
    format!(
        "label: gpt\ndevice: /dev/nvme0n1\nunit: sectors\nfirst-lba: 34\nsector-size: 512\n\n/dev/nvme0n1p1 : start={ROOT_START:>12}, size={root_size:>12}, type={LINUX_FS}\n/dev/nvme0n1p15 : start=2048, size=8192, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B\n"
    )
}

struct Fixture {
    guest: Arc<Mutex<Guest>>,
    system: Arc<FakeSystem>,
}

impl Fixture {
    fn new(guest: Guest) -> Self {
        let guest = Arc::new(Mutex::new(guest));
        let system = Arc::new(FakeSystem::default());
        let state = guest.clone();
        system.on(move |spec| {
            let argv: Vec<_> = spec.argv.iter().map(String::as_str).collect();
            let argv = argv.strip_prefix(&["sudo", "-n"]).unwrap_or(&argv);
            Some(Ok(state.lock().unwrap().command(argv)))
        });
        Self { guest, system }
    }

    fn run(&self, cloud_init: bool) -> Result<()> {
        let state = self.guest.clone();
        GrowRoot {
            system: self.system.clone(),
            probe: Box::new(move |_| Ok(state.lock().unwrap().device())),
            target_bytes: DISK_SECTORS * 512,
            cloud_init,
            prepared: None,
        }
        .run()
    }

    fn changes(&self) -> Vec<String> {
        self.system
            .calls()
            .into_iter()
            .filter_map(|call| {
                let call = call.trim_start_matches("sudo -n ");
                let program = call.split_whitespace().next().unwrap();
                (matches!(program, "growpart" | "partx" | "resize2fs")
                    && !call.contains("--dry-run"))
                .then(|| program.to_string())
            })
            .collect()
    }
}

#[test]
fn grows_the_last_physical_partition_and_is_idempotent() {
    let fixture = Fixture::new(Guest::default());
    fixture.run(true).unwrap();
    assert_eq!(fixture.changes(), ["growpart", "partx", "resize2fs"]);
    assert!(fixture.system.calls()[0].contains("cloud-init status --wait"));
    fixture.run(true).unwrap();
    assert_eq!(fixture.changes(), ["growpart", "partx", "resize2fs"]);
}

#[test]
fn accepts_completed_cloud_init_growth_and_disabled_cloud_init() {
    let fixture = Fixture::new(Guest {
        cloud_grows: true,
        cloud_code: 2,
        ..Guest::default()
    });
    fixture.run(true).unwrap();
    assert!(fixture.changes().is_empty());

    let fixture = Fixture::new(Guest {
        cloud_status: "disabled",
        ..Guest::default()
    });
    fixture.run(true).unwrap();
    assert_eq!(fixture.changes(), ["growpart", "partx", "resize2fs"]);
}

#[test]
fn rejects_wrong_disk_size_unsupported_layout_and_unfinished_cloud_init() {
    for guest in [
        Guest {
            disk_sectors: DISK_SECTORS - 1,
            ..Guest::default()
        },
        Guest {
            disk_sectors: DISK_SECTORS + 1,
            ..Guest::default()
        },
        Guest {
            boot_after_root: true,
            ..Guest::default()
        },
        Guest {
            bad_dry_run: true,
            ..Guest::default()
        },
        Guest {
            cloud_status: "running",
            ..Guest::default()
        },
        Guest {
            cloud_status: "error",
            cloud_code: 1,
            ..Guest::default()
        },
        Guest {
            fs_blocks: OLD_SIZE / 8 + 1,
            ..Guest::default()
        },
    ] {
        let fixture = Fixture::new(guest);
        assert!(fixture.run(true).is_err());
        assert!(fixture.changes().is_empty());
    }
}

#[test]
fn retries_forward_after_table_write_and_after_filesystem_failure() {
    let fixture = Fixture::new(Guest {
        fail_partx: true,
        fail_resize: true,
        ..Guest::default()
    });
    let error = fixture.run(false).unwrap_err().to_string();
    assert!(error.contains("kernel refused the update"));
    assert!(error.contains("changes are kept for retry"));
    assert_eq!(fixture.changes(), ["growpart", "partx"]);
    fixture.guest.lock().unwrap().fail_partx = false;
    assert!(
        fixture
            .run(false)
            .unwrap_err()
            .to_string()
            .contains("resize failed")
    );
    assert_eq!(
        fixture.changes(),
        ["growpart", "partx", "partx", "resize2fs"]
    );
    fixture.guest.lock().unwrap().fail_resize = false;
    fixture.run(false).unwrap();
    assert_eq!(
        fixture.changes(),
        ["growpart", "partx", "partx", "resize2fs", "resize2fs"]
    );
}

#[test]
fn verifies_filesystem_geometry_even_when_resize_reports_success() {
    let fixture = Fixture::new(Guest {
        ignore_resize: true,
        ..Guest::default()
    });
    let error = fixture.run(false).unwrap_err().to_string();
    assert!(error.contains("Ext4 root does not fill the expanded partition"));
}

#[test]
fn validates_growpart_output_geometry_and_status() {
    let device = Guest::default().device();
    let root = Partition {
        node: device.partition.clone(),
        start: ROOT_START,
        size: OLD_SIZE,
        kind: LINUX_FS.into(),
        metadata: BTreeMap::new(),
    };
    let valid = growth_report("CHANGE:", NEW_SIZE);
    let parse = |stdout, code| {
        Growth::parse(
            &Output {
                code: Some(code),
                stdout,
                stderr: String::new(),
            },
            "CHANGE:",
            &device,
            &root,
        )
    };
    assert!(parse(valid.clone(), 0).is_ok());
    assert!(parse(valid.clone(), 1).is_err());
    for invalid in [
        valid.replace("CHANGE:", "CHANGED:"),
        valid.replace("partition=1", "partition=11"),
        valid.replace(&format!("start={ROOT_START}"), "start=1"),
        valid.replace(&format!("end={}", ROOT_START + OLD_SIZE - 1), "end=0"),
        valid.replace(&format!("end={}", ROOT_START + NEW_SIZE - 1), "end=0"),
        growth_report("CHANGE:", OLD_SIZE - 1),
        growth_report("CHANGE:", NEW_SIZE - 2048),
        growth_report("CHANGE:", DISK_SECTORS - ROOT_START),
        format!("{valid}another result"),
    ] {
        assert!(parse(invalid, 0).is_err());
    }
}
