//! Live resource monitor: CPU, RAM, swap, load, latency and throughput,
//! plus the busiest processes on request (`top_processes`).
//!
//! Samples once per second on a background thread. Latency is a TCP connect
//! to a configurable `host:port` (no ICMP, so no privileges), measured every
//! five seconds on its own thread so a slow network never stalls sampling.
//! Unavailable metrics are `None`, never a made-up value.

use crate::util::LockExt;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sysinfo::{DiskRefreshKind, Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::model::{LoadAverage, MonitorSample, ProcessInfo, ProcessList};
use crate::util::now_millis;

pub fn machine_observation() -> crate::model::GuestMachineObservation {
    let disks = Disks::new_with_refreshed_list();
    let root = disks.iter().find(|disk| disk.mount_point() == std::path::Path::new("/"));
    crate::model::GuestMachineObservation {
        at: crate::util::now_millis(),
        kernel: System::kernel_version(),
        uptime_seconds: System::uptime(),
        root_total_bytes: root.map(sysinfo::Disk::total_space),
        root_available_bytes: root.map(sysinfo::Disk::available_space),
    }
}

pub fn tcp_latency(target: &str, timeout: Duration) -> Option<u32> {
    let address = crate::ipv4::resolve(target).ok()?.into_iter().next()?;
    let started = Instant::now();
    TcpStream::connect_timeout(&address, timeout).ok()?;
    Some(started.elapsed().as_millis().min(u32::MAX as u128) as u32)
}

pub struct Monitor {
    stop: Arc<AtomicBool>,
    latency_stop: std::sync::mpsc::Sender<()>,
    latest: Arc<Mutex<Option<MonitorSample>>>,
    /// Kept between requests: CPU use is the change since the last refresh.
    processes: Mutex<Option<(System, Instant)>>,
}

/// Command lines are shortened to this many characters.
const COMMAND_MAX: usize = 240;

fn process_info(pid: u32, process: &sysinfo::Process, cpus: f32) -> ProcessInfo {
    let mut command = process
        .cmd()
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    if command.chars().count() > COMMAND_MAX {
        command = command.chars().take(COMMAND_MAX).collect::<String>() + "…";
    }
    ProcessInfo {
        pid,
        name: process.name().to_string_lossy().into_owned(),
        command,
        cpu_percent: (process.cpu_usage() / cpus.max(1.0)).clamp(0.0, 100.0),
        memory: process.memory(),
        run_time: process.run_time(),
    }
}

/// The `limit` busiest processes by CPU and by memory (pure, tested).
fn rank(mut all: Vec<ProcessInfo>, limit: usize, at: i64) -> ProcessList {
    let total = all.len() as u32;
    all.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent).then(b.memory.cmp(&a.memory)));
    let by_cpu = all.iter().take(limit).cloned().collect();
    all.sort_by_key(|process| std::cmp::Reverse(process.memory));
    let by_memory = all.into_iter().take(limit).collect();
    ProcessList { at, total, by_cpu, by_memory }
}

impl Monitor {
    /// Start sampling. `target` is read before every latency probe so
    /// settings changes apply without a restart.
    pub fn start(
        target: impl Fn() -> String + Send + Sync + 'static,
        sink: impl Fn(MonitorSample) + Send + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let latest = Arc::new(Mutex::new(None));
        let latency: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));

        let (latency_stop, latency_wait) = std::sync::mpsc::channel();
        {
            let stop = stop.clone();
            let latency = latency.clone();
            let _ = std::thread::Builder::new()
                .name("monitor-latency".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let value = tcp_latency(&target(), Duration::from_secs(2));
                        *latency.locked() = value;
                        if latency_wait.recv_timeout(Duration::from_secs(5))
                            != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                        {
                            break;
                        }
                    }
                });
        }

        {
            let stop = stop.clone();
            let latest = latest.clone();
            let _ = std::thread::Builder::new()
                .name("monitor".into())
                .spawn(move || {
                    let mut system = System::new();
                    let mut networks = Networks::new_with_refreshed_list();
                    let io_only = DiskRefreshKind::nothing().with_io_usage();
                    let mut disks = Disks::new_with_refreshed_list_specifics(io_only);
                    system.refresh_cpu_usage();
                    let mut last = Instant::now();
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_secs(1));
                        system.refresh_cpu_usage();
                        system.refresh_memory();
                        networks.refresh(true);
                        disks.refresh_specifics(true, io_only);
                        let elapsed = last.elapsed().as_secs_f64().max(0.001);
                        last = Instant::now();
                        let (received, transmitted) = networks
                            .iter()
                            .filter(|(name, _)| name.as_str() != "lo")
                            .fold((0u64, 0u64), |(rx, tx), (_, data)| {
                                (rx + data.received(), tx + data.transmitted())
                            });
                        let has_network = networks.iter().any(|(name, _)| name.as_str() != "lo");
                        // One device can be mounted several times; count it once.
                        let mut seen = std::collections::HashSet::new();
                        let (read, written) = disks
                            .list()
                            .iter()
                            .filter(|disk| seen.insert(disk.name().to_os_string()))
                            .fold((0u64, 0u64), |(r, w), disk| {
                                let usage = disk.usage();
                                (r + usage.read_bytes, w + usage.written_bytes)
                            });
                        let has_disks = !disks.list().is_empty();
                        let total = system.total_memory();
                        let cpus = system.cpus().len();
                        let swap_total = system.total_swap();
                        let load = System::load_average();
                        let sample = MonitorSample {
                            at: now_millis(),
                            cpu_percent: (cpus > 0).then(|| system.global_cpu_usage()),
                            memory_used: (total > 0).then(|| system.used_memory()),
                            memory_total: (total > 0).then_some(total),
                            latency_ms: *latency.locked(),
                            download_bps: has_network.then(|| (received as f64 / elapsed) as u64),
                            upload_bps: has_network.then(|| (transmitted as f64 / elapsed) as u64),
                            disk_read_bps: has_disks.then(|| (read as f64 / elapsed) as u64),
                            disk_write_bps: has_disks.then(|| (written as f64 / elapsed) as u64),
                            swap_used: (swap_total > 0).then(|| system.used_swap()),
                            swap_total: (swap_total > 0).then_some(swap_total),
                            load: (cpus > 0).then_some(LoadAverage { one: load.one, five: load.five, fifteen: load.fifteen }),
                            cpu_count: (cpus > 0).then_some(cpus as u32),
                        };
                        *latest.locked() = Some(sample.clone());
                        sink(sample);
                    }
                });
        }

        Self { stop, latency_stop, latest, processes: Mutex::new(None) }
    }

    /// The busiest processes now. The first call takes two readings a short
    /// moment apart (CPU use needs an interval); later calls measure since
    /// the previous one.
    pub fn top_processes(&self, limit: usize) -> ProcessList {
        let kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_cmd(UpdateKind::OnlyIfNotSet);
        let mut guard = self.processes.locked();
        let fresh = guard.as_ref().is_none_or(|(_, at)| at.elapsed() > Duration::from_secs(30));
        if fresh {
            let mut system = System::new();
            system.refresh_cpu_usage();
            system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
            std::thread::sleep(Duration::from_millis(300));
            *guard = Some((system, Instant::now()));
        }
        let (system, at) = guard.as_mut().expect("process system is set");
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        *at = Instant::now();
        let cpus = system.cpus().len().max(1) as f32;
        let all = system
            .processes()
            .iter()
            .map(|(pid, process)| process_info(pid.as_u32(), process, cpus))
            .collect();
        rank(all, limit, now_millis())
    }

    pub fn latest(&self) -> Option<MonitorSample> {
        self.latest.locked()
            .clone()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.latency_stop.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_samples_with_real_memory_figures() {
        let (tx, rx) = std::sync::mpsc::channel();
        let monitor = Monitor::start(
            || "127.0.0.1:1".into(),
            move |sample| {
                let _ = tx.send(sample);
            },
        );
        let sample = rx.recv_timeout(Duration::from_secs(5)).expect("sample");
        assert!(sample.memory_total.unwrap_or_default() > 0);
        assert!(sample.memory_used.is_some());
        assert!(monitor.latest().is_some());
    }

    fn process(pid: u32, cpu: f32, memory: u64) -> ProcessInfo {
        ProcessInfo { pid, name: format!("p{pid}"), command: String::new(), cpu_percent: cpu, memory, run_time: 1 }
    }

    #[test]
    fn processes_are_ranked_by_cpu_and_by_memory() {
        let list = rank(vec![process(1, 5.0, 900), process(2, 40.0, 10), process(3, 0.0, 5000)], 2, 7);
        assert_eq!(list.total, 3);
        assert_eq!(list.by_cpu.iter().map(|p| p.pid).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(list.by_memory.iter().map(|p| p.pid).collect::<Vec<_>>(), [3, 1]);
    }

    #[test]
    fn this_process_is_listed() {
        let monitor = Monitor::start(|| "127.0.0.1:1".into(), |_| {});
        let list = monitor.top_processes(500);
        assert!(list.total > 0);
        assert!(list.by_memory.iter().any(|p| p.pid == std::process::id()));
    }
}
