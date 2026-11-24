//! Forward-only expansion for the Ubuntu cloud image's directly mounted ext4 root.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::model::OperationState;
use crate::ops::{Operation, OperationManager, Step, StepCtx};
use crate::store::Store;
use crate::system::{CommandSpec, Output, System, privileged};
use crate::{Error, Result};

const KERNEL_SECTOR: u64 = 512;
const ALIGNMENT: u64 = 1024 * 1024;
const GPT_BACKUP_SECTORS: u64 = 33; // Header and 128 entries on a 512-byte logical disk.
const QUERY_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(super) use linux::run;

fn spec<'a>(argv: impl IntoIterator<Item = &'a str>) -> CommandSpec {
    privileged(argv)
        .env([("LC_ALL".into(), "C".into())])
        .output_limit(1024 * 1024)
}

fn command_error(message: impl Into<String>, output: &Output) -> Error {
    Error::Command {
        message: message.into(),
        output: output.combined(),
    }
}

fn number(text: &str) -> Option<u64> {
    text.bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| text.parse().ok())
        .flatten()
}

fn wait_for_cloud_init(system: &dyn System) -> Result<()> {
    #[derive(Deserialize)]
    struct Status {
        status: String,
    }

    let output = system.run(
        &spec(["cloud-init", "status", "--wait", "--format", "json"])
            .timeout(Duration::from_secs(180)),
    )?;
    let status = serde_json::from_str::<Status>(&output.stdout).ok();
    // Exit 2 is a completed boot with recoverable errors on supported Ubuntu releases.
    match (
        output.code,
        status.as_ref().map(|status| status.status.as_str()),
    ) {
        (Some(0 | 2), Some("done")) | (Some(0), Some("disabled")) => Ok(()),
        _ => Err(command_error(
            "cloud-init did not report a completed or disabled boot",
            &output,
        )),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RootDevice {
    disk: String,
    partition: String,
    number: u64,
    disk_sectors: u64,
    kernel_start: u64,
    kernel_size: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Partition {
    node: String,
    start: u64,
    size: u64,
    #[serde(rename = "type")]
    kind: String,
    #[serde(flatten)]
    metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Table {
    label: String,
    id: String,
    device: String,
    unit: String,
    firstlba: u64,
    lastlba: u64,
    sectorsize: u64,
    partitions: Vec<Partition>,
}

impl Table {
    fn read(system: &dyn System, device: &RootDevice) -> Result<Self> {
        #[derive(Deserialize)]
        struct Report {
            partitiontable: Table,
        }
        let output =
            system.run(&spec(["sfdisk", "--json", &device.disk]).timeout(QUERY_TIMEOUT))?;
        // The old GPT backup remains at the previous end of the disk until growpart writes it.
        if !output.success() || !output.stderr.lines().all(|line| {
            line.is_empty()
                || line == "The backup GPT table is not on the end of the device."
                || line == "The backup GPT table is not on the end of the device. This problem will be corrected by write."
                || (line.starts_with("GPT PMBR size mismatch (") && line.ends_with(") will be corrected by write."))
        }) {
            return Err(command_error("Could not read a healthy root disk partition table", &output));
        }
        let mut table = serde_json::from_str::<Report>(&output.stdout)?.partitiontable;
        table.partitions.sort_by_key(|partition| partition.start);
        table.validate(device)?;
        // A completed growth must also leave both GPT headers at their proper locations.
        if table.at_disk_end(device) && !output.stderr.trim().is_empty() {
            return Err(command_error(
                "Root disk GPT still reports a misplaced backup",
                &output,
            ));
        }
        Ok(table)
    }

    fn validate(&self, device: &RootDevice) -> Result<()> {
        let unsupported = || {
            Error::invalid(
                "Root disk must have a standard GPT with ext4 as its last physical partition",
            )
        };
        if self.label != "gpt"
            || self.device != device.disk
            || self.unit != "sectors"
            || self.sectorsize != KERNEL_SECTOR
            || self.firstlba < GPT_BACKUP_SECTORS + 1
            || self.lastlba >= device.disk_sectors.saturating_sub(GPT_BACKUP_SECTORS)
        {
            return Err(unsupported());
        }
        let mut previous_end = self.firstlba;
        for (index, partition) in self.partitions.iter().enumerate() {
            let end = partition
                .start
                .checked_add(partition.size)
                .ok_or_else(unsupported)?;
            if partition.size == 0
                || partition.start < previous_end
                || end - 1 > self.lastlba
                || self.partitions[..index]
                    .iter()
                    .any(|other| other.node == partition.node)
            {
                return Err(unsupported());
            }
            previous_end = end;
        }
        let root = self.partitions.last().ok_or_else(unsupported)?;
        if root.node != device.partition
            || root.start != device.kernel_start
            || device.kernel_size == 0
            || device.kernel_size > root.size
            || ![
                "0FC63DAF-8483-4772-8E79-3D69D8477DE4", // Linux filesystem
                "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709", // Linux x86-64 root
                "B921B045-1DF0-41C3-AF44-4C6F280D3FAE", // Linux ARM64 root
            ]
            .iter()
            .any(|kind| root.kind.eq_ignore_ascii_case(kind))
        {
            return Err(unsupported());
        }
        Ok(())
    }

    // The validated root is last in sector order, irrespective of its partition number.
    fn root(&self) -> &Partition {
        self.partitions.last().expect("validated root partition")
    }

    fn at_disk_end(&self, device: &RootDevice) -> bool {
        self.lastlba + GPT_BACKUP_SECTORS + 1 == device.disk_sectors
    }
}

fn near_disk_end(device: &RootDevice, start: u64, size: u64) -> bool {
    let gap = start
        .checked_add(size)
        .and_then(|end| device.disk_sectors.checked_sub(end));
    // growpart leaves room for GPT and may round the partition size down to a MiB.
    gap.is_some_and(|gap| {
        (GPT_BACKUP_SECTORS..=GPT_BACKUP_SECTORS + ALIGNMENT / KERNEL_SECTOR).contains(&gap)
    })
}

#[derive(Debug, PartialEq, Eq)]
struct Growth {
    old_size: u64,
    new_size: u64,
}

impl Growth {
    fn parse(output: &Output, marker: &str, device: &RootDevice, root: &Partition) -> Result<Self> {
        let invalid = || {
            command_error(
                "growpart returned an unexpected root partition geometry",
                output,
            )
        };
        let fields: Vec<_> = output.stdout.split_whitespace().collect();
        let [
            prefix,
            part,
            start,
            "old:",
            old_size,
            old_end,
            "new:",
            new_size,
            new_end,
        ] = fields.as_slice()
        else {
            return Err(invalid());
        };
        let field = |field: &str, name| {
            field
                .strip_prefix(name)
                .and_then(number)
                .ok_or_else(invalid)
        };
        let start = field(start, "start=")?;
        let old_size = field(old_size, "size=")?;
        let new_size = field(new_size, "size=")?;
        let end = |size| start.checked_add(size).and_then(|end| end.checked_sub(1));
        if !output.success()
            || *prefix != marker
            || field(part, "partition=")? != device.number
            || start != root.start
            || old_size != root.size
            || new_size <= old_size
            || end(old_size) != Some(field(old_end, "end=")?)
            || end(new_size) != Some(field(new_end, "end=")?)
            || !near_disk_end(device, start, new_size)
        {
            return Err(invalid());
        }
        Ok(Self { old_size, new_size })
    }

    fn check_dump(&self, output: &Output, partition: &str) -> Result<()> {
        let invalid = || {
            command_error(
                "growpart dry run changed more than the root partition size",
                output,
            )
        };
        let (_, dumps) = output
            .stderr
            .split_once("# === old sfdisk -d ===\n")
            .ok_or_else(invalid)?;
        let (old, new) = dumps
            .split_once("# === new sfdisk -d ===\n")
            .ok_or_else(invalid)?;
        let summarize = |dump: &str, expected_size| {
            let mut found = false;
            let mut lines = Vec::new();
            for line in dump.lines() {
                if let Some((node, fields)) = line.split_once(':')
                    && node.trim() == partition
                {
                    let (start, remainder) = fields.split_once(',').ok_or_else(invalid)?;
                    let (size, rest) = remainder.split_once(',').ok_or_else(invalid)?;
                    if found
                        || size
                            .trim()
                            .strip_prefix("size=")
                            .and_then(|value| number(value.trim()))
                            != Some(expected_size)
                    {
                        return Err(invalid());
                    }
                    lines.push(format!("{node}:{start},size=,{rest}"));
                    found = true;
                } else {
                    lines.push(line.to_owned());
                }
            }
            if !found {
                return Err(invalid());
            }
            Ok(lines)
        };
        if summarize(old, self.old_size)? != summarize(new, self.new_size)? {
            return Err(invalid());
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Ext4 {
    blocks: u64,
    block_size: u64,
    large: bool,
}

impl Ext4 {
    fn read(system: &dyn System, device: &RootDevice) -> Result<Self> {
        let output =
            system.check(&spec(["dumpe2fs", "-h", &device.partition]).timeout(QUERY_TIMEOUT))?;
        Self::parse(&output, device.kernel_size)
    }

    fn parse(output: &str, kernel_size: u64) -> Result<Self> {
        let invalid = || Error::invalid("Unsupported ext4 root filesystem geometry or features");
        let field = |name| {
            let mut values = output
                .lines()
                .filter_map(|line| line.strip_prefix(name).map(str::trim));
            let value = values.next().ok_or_else(invalid)?;
            if values.next().is_some() {
                return Err(invalid());
            }
            Ok(value)
        };
        let blocks = number(field("Block count:")?).ok_or_else(invalid)?;
        let block_size = number(field("Block size:")?).ok_or_else(invalid)?;
        let features = field("Filesystem features:")?;
        let has = |feature: &str| features.split_whitespace().any(|value| value == feature);
        if !(1024..=65536).contains(&block_size)
            || !block_size.is_power_of_two()
            || blocks == 0
            || has("bigalloc")
            || has("sparse_super2")
            || field("Filesystem state:")? != "clean"
            || blocks > kernel_size / (block_size / KERNEL_SECTOR)
        {
            return Err(invalid());
        }
        Ok(Self {
            blocks,
            block_size,
            large: has("64bit"),
        })
    }

    fn capacity(&self, partition_sectors: u64) -> Result<u64> {
        let capacity = partition_sectors / (self.block_size / KERNEL_SECTOR);
        if !self.large && capacity > u64::from(u32::MAX) {
            return Err(Error::invalid(
                "Requested growth exceeds the ext4 filesystem's block count limit",
            ));
        }
        Ok(capacity)
    }
}

struct Snapshot {
    device: RootDevice,
    table: Table,
    fs: Ext4,
}

impl Snapshot {
    fn verify(&self) -> Result<()> {
        let root = self.table.root();
        if !self.table.at_disk_end(&self.device)
            || !near_disk_end(&self.device, root.start, root.size)
            || root.size != self.device.kernel_size
        {
            return Err(Error::Conflict(
                "Root partition does not span the expanded disk in the GPT and kernel".into(),
            ));
        }
        if self.fs.blocks != self.fs.capacity(root.size)? {
            return Err(Error::Conflict(
                "Ext4 root does not fill the expanded partition".into(),
            ));
        }
        Ok(())
    }
}

type Probe = Box<dyn Fn(&dyn System) -> Result<RootDevice> + Send>;

struct GrowRoot {
    system: Arc<dyn System>,
    probe: Probe,
    target_bytes: u64,
    cloud_init: bool,
    prepared: Option<(Snapshot, Option<Growth>)>,
}

impl GrowRoot {
    fn snapshot(&self) -> Result<Snapshot> {
        let device = (self.probe)(self.system.as_ref())?;
        let bytes = device
            .disk_sectors
            .checked_mul(KERNEL_SECTOR)
            .ok_or_else(|| Error::invalid("Root disk byte count overflow"))?;
        if bytes != self.target_bytes {
            return Err(Error::Conflict(format!(
                "Root disk has {bytes} bytes, expected exactly {}",
                self.target_bytes
            )));
        }
        let table = Table::read(self.system.as_ref(), &device)?;
        let fs = Ext4::read(self.system.as_ref(), &device)?;
        Ok(Snapshot { device, table, fs })
    }

    fn growpart(&self, snapshot: &Snapshot, dry_run: bool) -> Result<Growth> {
        let device = &snapshot.device;
        let number = device.number.to_string();
        // Update the kernel separately so a partx failure cannot trigger growpart's own rollback.
        let mut argv = vec!["growpart", "--fudge", "0", "--update", "off"];
        if dry_run {
            argv.push("--dry-run");
        }
        argv.extend([device.disk.as_str(), &number]);
        let mut command = spec(argv).env([("GROWPART_RESIZER".into(), "sfdisk".into())]);
        if dry_run {
            command = command.timeout(QUERY_TIMEOUT);
        }
        let output = self.system.run(&command)?;
        let marker = if dry_run { "CHANGE:" } else { "CHANGED:" };
        let growth = Growth::parse(&output, marker, device, snapshot.table.root())?;
        if dry_run {
            growth.check_dump(&output, &device.partition)?;
        }
        Ok(growth)
    }

    fn check_after(before: &Snapshot, after: &Snapshot, size: u64) -> Result<()> {
        let mut expected = before.table.partitions.clone();
        expected.last_mut().expect("validated root partition").size = size;
        if before.device.disk != after.device.disk
            || before.device.partition != after.device.partition
            || before.device.number != after.device.number
            || before.table.id != after.table.id
            || before.table.firstlba != after.table.firstlba
            || before.fs.block_size != after.fs.block_size
            || before.fs.large != after.fs.large
            || before.fs.blocks > after.fs.blocks
            || expected != after.table.partitions
            || !after.table.at_disk_end(&after.device)
            || !near_disk_end(&after.device, after.table.root().start, size)
        {
            return Err(Error::Conflict(
                "Root disk geometry differs from the validated growth plan".into(),
            ));
        }
        Ok(())
    }

    fn run(self) -> Result<()> {
        // This root helper runs before the user daemon and does not open its database.
        let ops = OperationManager::new(Arc::new(Store::in_memory()?));
        let record = ops.run(Operation::new("growRoot", "Grow guest root disk").step(self))?;
        if record.state == OperationState::Succeeded {
            return Ok(());
        }
        let message = record
            .error
            .unwrap_or_else(|| "Could not grow guest root disk".into());
        Err(Error::Command {
            message: format!("{message}\n{}", record.steps[0].log.join("\n")),
            output: String::new(),
        })
    }
}

impl Step for GrowRoot {
    fn name(&self) -> String {
        "Grow the ext4 root partition and filesystem".into()
    }

    fn forward_only(&self) -> bool {
        true
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        if self.cloud_init {
            wait_for_cloud_init(self.system.as_ref())?;
        }
        let before = self.snapshot()?;
        let root = before.table.root();
        let growth = if before.table.at_disk_end(&before.device)
            && near_disk_end(&before.device, root.start, root.size)
        {
            None
        } else {
            // Begin by reconciling an earlier write with the kernel. Never write a second
            // partition table while the mounted kernel view is already lagging behind it.
            if root.size != before.device.kernel_size {
                return Err(Error::Conflict(
                    "Root GPT and kernel disagree before partition growth".into(),
                ));
            }
            Some(self.growpart(&before, true)?)
        };
        before
            .fs
            .capacity(growth.as_ref().map_or(root.size, |growth| growth.new_size))?;
        self.prepared = Some((before, growth));
        Ok(())
    }

    fn apply(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        let (before, growth) = self.prepared.as_ref().expect("growth has been prepared");
        let mut current = self.snapshot()?;
        if before.device != current.device
            || before.table != current.table
            || before.fs != current.fs
        {
            return Err(Error::Conflict(
                "Root disk geometry changed during growth preparation".into(),
            ));
        }
        let size = if let Some(growth) = growth {
            if self.growpart(before, false)? != *growth {
                return Err(Error::Conflict(
                    "growpart applied a different change than its dry run".into(),
                ));
            }
            current = self.snapshot()?;
            growth.new_size
        } else {
            before.table.root().size
        };
        Self::check_after(before, &current, size)?;
        if current.device.kernel_size < size {
            let part = current.device.number.to_string();
            self.system.check(
                &spec(["partx", "--update", "--nr", &part, &current.device.disk])
                    .timeout(QUERY_TIMEOUT),
            )?;
            current = self.snapshot()?;
            Self::check_after(before, &current, size)?;
        }
        if current.device.kernel_size != size {
            return Err(Error::Conflict(
                "Kernel has not adopted the expanded root partition".into(),
            ));
        }
        let blocks = current.fs.capacity(size)?;
        if current.fs.blocks < blocks {
            self.system.check(&spec([
                "resize2fs",
                &current.device.partition,
                &blocks.to_string(),
            ]))?;
        }
        Ok(())
    }

    fn verify(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        let (before, growth) = self.prepared.as_ref().expect("growth has been prepared");
        let after = self.snapshot()?;
        Self::check_after(
            before,
            &after,
            growth
                .as_ref()
                .map_or(before.table.root().size, |growth| growth.new_size),
        )?;
        after.verify()
    }
}

#[cfg(test)]
mod tests;
