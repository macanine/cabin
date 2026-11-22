//! Active-route reachability and the retry budget for holding guest traffic.

use std::io::Read;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use agent_core::model::{NetworkConnectivity, NetworkTraffic};
use vm_core::network_probe_targets::{ALTERNATE_HTTPS_URL, HTTPS_URL};

pub const CHECK_INTERVAL: Duration = Duration::from_secs(15);
pub const RETRY_INTERVAL: Duration = Duration::from_millis(150);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const FAILURE_LIMIT: u32 = 3;
pub(crate) const HOLD_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldCheck {
    Reachability,
    Guest,
    Verification,
}

#[derive(Debug, Default)]
pub struct Connectivity {
    pub mode: NetworkConnectivity,
    pub failed_checks: u32,
    pub error: Option<String>,
    pub check: Option<HoldCheck>,
    deadline: Option<Instant>,
}

impl Connectivity {
    pub fn hold(&mut self, check: HoldCheck, reason: String) -> bool {
        if self.mode != NetworkConnectivity::Normal {
            return false;
        }
        self.mode = NetworkConnectivity::Rechecking;
        self.check = Some(check);
        self.deadline = Some(Instant::now() + HOLD_TIMEOUT);
        self.error = Some(reason);
        true
    }

    pub fn fail(&mut self, reason: String) -> bool {
        if self.mode != NetworkConnectivity::Rechecking {
            return false;
        }
        self.failed_checks += 1;
        self.error = Some(reason);
        if self.failed_checks == FAILURE_LIMIT {
            self.block();
        }
        true
    }

    fn block(&mut self) {
        self.mode = NetworkConnectivity::Blocked;
        self.check = None;
        self.deadline = None;
    }

    pub fn expire(&mut self, now: Instant) -> bool {
        if self.deadline.is_none_or(|deadline| now < deadline) {
            return false;
        }
        self.block();
        self.error = Some("The network recheck timed out".into());
        true
    }

    pub fn clear(&mut self) -> bool {
        let changed = self.mode != NetworkConnectivity::Normal;
        *self = Self::default();
        changed
    }
}

/// A trace confirms the exit address. The independent 204 check can still
/// establish reachability when neither trace endpoint answers.
pub fn probe(proxy_url: &str) -> Result<Option<Ipv4Addr>, String> {
    let proxy = ureq::Proxy::new(proxy_url).map_err(|_| "Invalid gateway listener address")?;
    let agent = ureq::AgentBuilder::new()
        .resolver(agent_core::ipv4::resolve)
        .try_proxy_from_env(false)
        .proxy(proxy)
        .redirects(0)
        .timeout(REQUEST_TIMEOUT)
        .timeout_connect(REQUEST_TIMEOUT)
        .build();
    std::thread::scope(|scope| {
        let fallback = scope.spawn(|| trace_exit(&agent, ALTERNATE_HTTPS_URL));
        let reachable = scope.spawn(|| {
            agent.get("https://www.gstatic.com/generate_204").call().is_ok_and(|response| response.status() == 204)
        });
        let address = trace_exit(&agent, HTTPS_URL).or_else(|| fallback.join().ok().flatten());
        if address.is_some() {
            return Ok(address);
        }
        if reachable.join().unwrap_or(false) {
            Ok(None)
        } else {
            Err("No HTTPS connectivity target responded through the current route".into())
        }
    })
}

fn trace_exit(agent: &ureq::Agent, url: &str) -> Option<Ipv4Addr> {
    let response = agent.get(url).call().ok()?;
    if response.status() != 200 {
        return None;
    }
    let mut text = String::new();
    response.into_reader().take(8192).read_to_string(&mut text).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("ip="))?
        .trim()
        .parse()
        .ok()
}

pub fn traffic_sample(
    session: u64,
    now: i64,
    rx_bytes: u64,
    tx_bytes: u64,
    previous: Option<&NetworkTraffic>,
) -> NetworkTraffic {
    let rate = |count: u64, previous: u64, elapsed: i64| {
        let delta = u128::from(count.saturating_sub(previous));
        let rate = delta * 1000 / elapsed as u128;
        u64::try_from(rate).unwrap_or(u64::MAX)
    };
    let comparison = previous.filter(|sample| {
        sample.gateway_session == session
            && (1..=2500).contains(&now.saturating_sub(sample.sampled_at))
            && rx_bytes >= sample.rx_bytes
            && tx_bytes >= sample.tx_bytes
    });
    NetworkTraffic {
        gateway_session: session,
        sampled_at: now,
        rx_bytes,
        tx_bytes,
        rx_bps: comparison.map(|prior| rate(rx_bytes, prior.rx_bytes, now - prior.sampled_at)),
        tx_bps: comparison.map(|prior| rate(tx_bytes, prior.tx_bytes, now - prior.sampled_at)),
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, Ordering};

    use block2::{Block, RcBlock};
    use dispatch2::{DispatchQueue, DispatchRetained};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceWillSleepNotification};
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};

    struct PowerWatch {
        center: Retained<NSNotificationCenter>,
        observers: [Retained<ProtocolObject<dyn NSObjectProtocol>>; 2],
    }

    impl Drop for PowerWatch {
        fn drop(&mut self) {
            for observer in &self.observers {
                // SAFETY: these tokens were registered on this center.
                unsafe { self.center.removeObserver(observer.as_ref() as &AnyObject) };
            }
        }
    }

    thread_local! {
        static POWER_WATCH: RefCell<Option<PowerWatch>> = const { RefCell::new(None) };
    }

    /// Called from Tauri's main-thread setup, before starting the VM.
    pub fn install_power_watch(on_change: impl Fn(bool) + Clone + Send + Sync + 'static) {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let observers = [true, false].map(|sleeping| {
            let change = on_change.clone();
            let block = RcBlock::new(move |_: NonNull<NSNotification>| change(sleeping));
            // SAFETY: the names are AppKit constants. A nil queue makes the
            // cutoff synchronous with the notification and the center owns the block.
            unsafe {
                let name = if sleeping {
                    NSWorkspaceWillSleepNotification
                } else {
                    NSWorkspaceDidWakeNotification
                };
                center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
            }
        });
        POWER_WATCH.set(Some(PowerWatch { center, observers }));
    }

    #[link(name = "Network", kind = "framework")]
    unsafe extern "C" {
        fn nw_path_monitor_create() -> *mut c_void;
        fn nw_path_monitor_set_queue(monitor: *mut c_void, queue: *mut DispatchQueue);
        fn nw_path_monitor_set_update_handler(
            monitor: *mut c_void,
            handler: *mut Block<dyn Fn(*mut c_void)>,
        );
        fn nw_path_monitor_start(monitor: *mut c_void);
        fn nw_path_monitor_cancel(monitor: *mut c_void);
        fn nw_release(object: *mut c_void);
    }

    pub struct PathWatch(NonNull<c_void>);

    impl PathWatch {
        pub fn new(on_change: impl Fn() + Send + Sync + 'static) -> Option<Self> {
            // The monitor retains the queue and copies the callback block. The
            // monitor and its cancellation stay on the worker that owns it.
            let monitor = NonNull::new(unsafe { nw_path_monitor_create() })?;
            let queue = DispatchQueue::new("cabin.network-path", None);
            let initialized = AtomicBool::new(false);
            let handler = RcBlock::new(move |_: *mut c_void| {
                if initialized.swap(true, Ordering::Relaxed) {
                    on_change();
                }
            });
            unsafe {
                nw_path_monitor_set_queue(
                    monitor.as_ptr(),
                    DispatchRetained::as_ptr(&queue).as_ptr(),
                );
                nw_path_monitor_set_update_handler(monitor.as_ptr(), RcBlock::as_ptr(&handler));
                nw_path_monitor_start(monitor.as_ptr());
            }
            Some(Self(monitor))
        }
    }

    impl Drop for PathWatch {
        fn drop(&mut self) {
            unsafe {
                nw_path_monitor_cancel(self.0.as_ptr());
                nw_release(self.0.as_ptr());
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::{PathWatch, install_power_watch};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspension_recovers_and_hard_block_requires_three_failures() {
        let mut path = Connectivity::default();
        assert!(path.hold(HoldCheck::Reachability, "path changed".into()));
        assert_eq!(path.failed_checks, 0);
        assert!(path.fail("one".into()));
        assert!(path.fail("two".into()));
        assert_eq!(path.mode, NetworkConnectivity::Rechecking);
        assert!(path.clear());
        assert_eq!(path.mode, NetworkConnectivity::Normal);
        assert_eq!(path.failed_checks, 0);
        assert!(path.hold(HoldCheck::Verification, "verification failed".into()));
        for attempt in 1..=3 {
            assert!(path.fail(format!("attempt {attempt}")));
            assert_eq!(path.failed_checks, attempt);
        }
        assert_eq!(path.mode, NetworkConnectivity::Blocked);
        assert!(!path.fail("still down".into()));
        assert_eq!(path.failed_checks, 3);
        assert!(path.clear());
    }

    #[test]
    fn rates_compare_samples_only_within_a_gateway_session() {
        let initial = traffic_sample(1, 1000, 1200, 700, None);
        assert_eq!(initial.rx_bps, None);
        let next = traffic_sample(1, 1500, 1500, 900, Some(&initial));
        assert_eq!((next.rx_bps, next.tx_bps), (Some(600), Some(400)));
        assert_eq!(traffic_sample(2, 2000, 100, 50, Some(&next)).rx_bps, None);
        assert_eq!(traffic_sample(1, 5000, 2000, 1000, Some(&next)).rx_bps, None);
        assert_eq!(traffic_sample(1, 2000, 1000, 999, Some(&next)).tx_bps, None);
    }
}
