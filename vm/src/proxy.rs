//! Embedded shoes proxy lifecycle.
//!
//! shoes is the only proxy engine. Its binary and default configuration live
//! with the project resources; runtime state and logs live under vmctl home.

use std::fs::OpenOptions;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::lock::FileLock;
use crate::paths::StatePaths;
use crate::util::{
    atomic_write_json, capture, ensure_dir, now_millis, pid_alive, read_json, remove_path,
};
use crate::{Error, Result};

pub const DEFAULT_LISTEN: &str = "127.0.0.1:18080";
pub const DEFAULT_GUEST_URL: &str = "http://127.0.0.1:18080";
const CONNECTIVITY_PROBE_URL: &str = "https://www.gstatic.com/generate_204";
// A public IP literal avoids DNS/synthetic-address rejection in the shoes
// controller. Any HTTP response is sufficient for a delay measurement.
const NODE_PROBE_URL: &str = "https://1.1.1.1/cdn-cgi/trace";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxyStatus {
    pub engine: String,
    pub version: String,
    pub embedded: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub listen: String,
    pub guest_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxyProbe {
    pub ok: bool,
    pub delay_ms: Option<u64>,
    pub http_status: Option<u16>,
    pub error: Option<String>,
    pub checked_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxyCheck {
    pub ok: bool,
    pub engine: String,
    pub version: String,
    pub config: PathBuf,
    pub output: String,
}

fn proxy_dir(home: &Path) -> PathBuf {
    StatePaths::new(home).proxy()
}

fn runtime_path(home: &Path) -> PathBuf {
    StatePaths::new(home).proxy_runtime()
}

fn log_path(home: &Path) -> PathBuf {
    StatePaths::new(home).proxy_log()
}

fn binary_path() -> PathBuf {
    crate::shoes::binary_path()
}

fn operation_lock(home: &Path) -> Result<FileLock> {
    FileLock::acquire(&StatePaths::new(home).proxy_transaction_lock())
}

pub fn should_manage(url: Option<&str>) -> bool {
    url.and_then(|value| url::Url::parse(value).ok())
        .is_some_and(|url| {
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
                && url.port_or_known_default() == Some(18080)
        })
}

/// Restricted slirp provides the guest only an SSH management forward and a
/// single connection to the embedded proxy. DNS/public/LAN packets cannot
/// leave through slirp, even if the guest changes its own firewall.
pub fn user_netdev(cfg: &crate::config::Config, id: &str) -> Result<String> {
    let mut value = format!("user,id={id},hostfwd=tcp:127.0.0.1:{}-:22", cfg.ssh_port);
    if should_manage(cfg.proxy.as_deref()) {
        let executable = std::env::current_exe()?;
        let executable = executable
            .to_str()
            .ok_or_else(|| Error::Message("executable path is not UTF-8".into()))?;
        let command = format!(
            "{} --vmctl-proxy-relay",
            crate::util::shell_quote(executable)
        )
        .replace(',', ",,");
        value.push_str(&format!(
            ",restrict=on,ipv6=off,guestfwd=tcp:10.0.2.2:18080-cmd:{command}"
        ));
    }
    Ok(value)
}

pub fn restricted_runtime(runtime: &crate::vm::Runtime) -> bool {
    let Some(pid) = runtime.pid() else {
        return false;
    };
    process_command_line(pid)
        .is_some_and(|command| {
            command.contains("restrict=on") && command.contains("--vmctl-proxy-relay")
        })
}

fn state(home: &Path) -> Value {
    read_json(&runtime_path(home)).unwrap_or_else(|| json!({}))
}

fn pid(value: &Value) -> Option<u32> {
    value
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn process_matches(runtime: &Value, pid: u32) -> bool {
    let Some(config) = runtime.get("config").and_then(Value::as_str) else {
        return false;
    };
    if runtime.get("engine").and_then(Value::as_str) != Some("shoes") {
        return false;
    }
    let expected = binary_path();
    process_command_line(pid).is_some_and(|command| {
        command.contains(&*expected.to_string_lossy()) && command.contains(config)
    })
}

/// The command line of a process, or None when it does not exist.
fn process_command_line(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .ok()
            .filter(|out| out.status.success())?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }
    #[cfg(windows)]
    {
        let script = format!(
            "(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine",
        );
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-Command", &script]);
        crate::compat::no_window(&mut command);
        let output = command
            .output()
            .ok()
            .filter(|result| result.status.success())?;
        let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Some(line).filter(|line| !line.is_empty())
    }
}

fn write_runtime(home: &Path, runtime: Value) -> Result<Value> {
    ensure_dir(&proxy_dir(home))?;
    atomic_write_json(&runtime_path(home), &runtime, 0o600)?;
    Ok(runtime)
}

pub fn check(home: &Path) -> Result<ProxyCheck> {
    let binary = binary_path();
    if !binary.is_file() {
        return Err(Error::Message(format!(
            "embedded shoes binary not found: {}",
            binary.display()
        )));
    }
    let path = managed_config(home)?;
    if !path.is_file() {
        return Err(Error::Message(format!(
            "shoes config not found: {}",
            path.display()
        )));
    }
    let result = capture(
        &[
            binary.display().to_string(),
            "check".to_string(),
            path.display().to_string(),
        ],
        Duration::from_secs(30),
        None,
    );
    if result.code != 0 {
        return Err(Error::Message(format!(
            "shoes config check failed: {}",
            result.output.trim()
        )));
    }
    Ok(ProxyCheck {
        ok: true,
        engine: "shoes".into(),
        version: version(),
        config: path,
        output: result.output.trim().into(),
    })
}

fn probe_error(message: impl Into<String>) -> ProxyProbe {
    ProxyProbe {
        ok: false,
        delay_ms: None,
        http_status: None,
        error: Some(message.into()),
        checked_at: now_millis(),
    }
}

fn brief_output(output: &str) -> String {
    let text = output.trim();
    let mut result: String = text.chars().take(240).collect();
    if text.chars().count() > 240 {
        result.push('…');
    }
    result
}

fn global_probe_failure(code: i32, output: &str) -> String {
    if code == 124 || output.contains("Operation timed out") {
        "Connection timed out".into()
    } else if output.contains("(56)") {
        "The proxy closed the connection before the test finished.".into()
    } else if output.contains("(7)") {
        "The local proxy listener is unreachable.".into()
    } else if output.contains("(5)") {
        "The proxy could not resolve an upstream address.".into()
    } else if output.contains("(35)") {
        "The upstream TLS handshake failed.".into()
    } else {
        "Unable to reach the test endpoint".into()
    }
}

fn probe_global(home: &Path) -> Result<ProxyProbe> {
    let current = status(home)?;
    if !current.running {
        return Ok(probe_error("Proxy is stopped"));
    }
    let proxy = format!("http://{}", current.listen);
    let result = capture(
        &[
            crate::compat::curl_binary().into(),
            "-q".into(),
            "--silent".into(),
            "--show-error".into(),
            "--max-time".into(),
            "8".into(),
            "--proxy".into(),
            proxy,
            "--output".into(),
            "/dev/null".into(),
            "--write-out".into(),
            "%{http_code} %{time_total}".into(),
            CONNECTIVITY_PROBE_URL.into(),
        ],
        Duration::from_secs(10),
        None,
    );
    let mut parts = result.output.split_whitespace();
    let status_code = parts.next().and_then(|value| value.parse::<u16>().ok());
    let delay_ms = parts
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .map(|seconds| (seconds * 1000.0).round() as u64);
    if result.code != 0 {
        return Ok(probe_error(global_probe_failure(
            result.code,
            &result.output,
        )));
    }
    let (Some(status_code), Some(delay_ms)) = (status_code, delay_ms) else {
        return Ok(probe_error("Unable to reach the test endpoint"));
    };
    if status_code == 0 {
        return Ok(probe_error("Unable to reach the test endpoint"));
    }
    Ok(ProxyProbe {
        ok: true,
        delay_ms: Some(delay_ms),
        http_status: Some(status_code),
        error: None,
        checked_at: now_millis(),
    })
}

fn probe_node(home: &Path, node: &str) -> Result<ProxyProbe> {
    let current = status(home)?;
    if !current.running {
        return Ok(probe_error("Proxy is stopped"));
    }
    if node.trim().is_empty() || node.len() > 512 {
        return Ok(probe_error("Invalid node"));
    }
    let Some(config) = state(home)
        .get("config")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(probe_error("Proxy configuration is unavailable"));
    };
    let result = match crate::shoes::node_delay(Path::new(&config), node, NODE_PROBE_URL, 5000) {
        Ok(result) => result,
        Err(error) => return Ok(probe_error(error.to_string())),
    };
    let delay = result.get("delay").and_then(Value::as_u64).or_else(|| {
        result
            .get("history")
            .and_then(Value::as_array)?
            .last()?
            .get("delay")?
            .as_u64()
    });
    match delay {
        Some(delay) => Ok(ProxyProbe {
            ok: true,
            delay_ms: Some(delay),
            http_status: None,
            error: None,
            checked_at: now_millis(),
        }),
        None => {
            let message = result
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| brief_output(&result.to_string()));
            Ok(probe_error(if message.is_empty() {
                "Node is unavailable"
            } else {
                &message
            }))
        }
    }
}

pub fn probe(home: &Path, node: Option<&str>) -> Result<ProxyProbe> {
    match node {
        Some(node) => probe_node(home, node),
        None => probe_global(home),
    }
}

pub fn version() -> String {
    let binary = binary_path();
    let result = capture(
        &[binary.display().to_string(), "--version".to_string()],
        Duration::from_secs(10),
        None,
    );
    result
        .output
        .lines()
        .next()
        .unwrap_or("shoes unavailable")
        .trim()
        .to_string()
}

pub fn status(home: &Path) -> Result<ProxyStatus> {
    let runtime = state(home);
    let pid = pid(&runtime);
    let running = pid.is_some_and(|pid| pid_alive(pid) && process_matches(&runtime, pid));
    if pid.is_some()
        && !running
        && let Ok(_lock) = operation_lock(home)
        && state(home) == runtime
    {
        let _ = remove_path(&runtime_path(home));
    }
    let current = if running { runtime } else { Value::Null };
    Ok(ProxyStatus {
        engine: "shoes".into(),
        version: version(),
        embedded: binary_path().is_file(),
        running,
        pid: running.then_some(pid).flatten(),
        listen: current
            .get("listen")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_LISTEN)
            .into(),
        guest_url: current
            .get("guest_url")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_GUEST_URL)
            .into(),
    })
}

pub fn start(home: &Path) -> Result<ProxyStatus> {
    let _lock = operation_lock(home)?;
    start_unlocked(home)
}

fn managed_config(home: &Path) -> Result<PathBuf> {
    let selected = crate::shoes::selected(home)
        .ok_or_else(|| Error::Message("Choose a global exit node first".into()))?;
    if !crate::shoes::selection_valid(home, &selected)? {
        return Err(Error::Message(
            "The selected global exit node no longer exists".into(),
        ));
    }
    let path = crate::shoes::config_path(home);
    if !path.is_file() {
        return Err(Error::Message(
            "Apply the selected node before starting the global proxy".into(),
        ));
    }
    Ok(path)
}

fn start_unlocked(home: &Path) -> Result<ProxyStatus> {
    let before = status(home)?;
    if before.running {
        return Ok(before);
    }
    let binary = binary_path();
    if !binary.is_file() {
        return Err(Error::Message(format!(
            "embedded proxy binary not found: {}",
            binary.display()
        )));
    }
    let selected_config = managed_config(home)?;
    check(home)?;
    let configured_listen = format!("127.0.0.1:{}", crate::shoes::listen_port());
    let port = configured_listen
        .rsplit(':')
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or_else(|| Error::Message("invalid proxy listener port".into()))?;
    if !crate::util::port_free(port, "127.0.0.1") {
        return Err(Error::Message(format!(
            "proxy port {port} is already in use"
        )));
    }
    ensure_dir(&proxy_dir(home))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(home))?;
    let log_err = log.try_clone()?;
    let child = Command::new(&binary)
        .arg(&selected_config)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .map_err(|error| Error::Message(format!("start proxy failed: {error}")))?;
    let runtime = json!({
        "engine": "shoes",
        "pid": child.id(),
        "listen": &configured_listen,
        "guest_url": format!("http://127.0.0.1:{}", configured_listen.rsplit(':').next().unwrap_or("18080")),
        "config": selected_config,
        "log": log_path(home),
        "started_at": now_millis(),
    });
    write_runtime(home, runtime)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut after = status(home)?;
    let mut ready = false;
    while Instant::now() < deadline {
        if !after.running {
            break;
        }
        if TcpStream::connect_timeout(&address, Duration::from_millis(150)).is_ok() {
            ready = true;
            break;
        }
        thread::sleep(Duration::from_millis(80));
        after = status(home)?;
    }
    if !ready {
        let _ = stop_unlocked(home, false);
        return Err(Error::Message(format!(
            "proxy did not open its listener; see {}",
            log_path(home).display()
        )));
    }
    Ok(after)
}

pub fn ensure_started(home: &Path) -> Result<ProxyStatus> {
    let current = status(home)?;
    if current.running {
        return Ok(current);
    }
    start(home)
}

pub fn stop(home: &Path, force: bool) -> Result<ProxyStatus> {
    let _lock = operation_lock(home)?;
    stop_unlocked(home, force)
}

fn stop_unlocked(home: &Path, force: bool) -> Result<ProxyStatus> {
    let current = status(home)?;
    let Some(pid) = current.pid.and_then(|value| i32::try_from(value).ok()) else {
        return Ok(current);
    };
    crate::compat::terminate_process(pid as u32, !force);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && pid_alive(pid as u32) {
        thread::sleep(Duration::from_millis(60));
    }
    if pid_alive(pid as u32) {
        crate::compat::terminate_process(pid as u32, false);
    }
    let _ = remove_path(&runtime_path(home));
    status(home)
}

pub fn log_path_for(home: &Path) -> PathBuf {
    log_path(home)
}

#[cfg(test)]
mod tests {

    #[test]
    fn stale_status_cleanup_respects_proxy_transactions() {
        let home = std::env::temp_dir().join(format!(
            "vmctl-proxy-status-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        super::write_runtime(&home, serde_json::json!({"pid": 0})).unwrap();
        let lock = super::operation_lock(&home).unwrap();
        assert!(!super::status(&home).unwrap().running);
        assert!(super::runtime_path(&home).exists());
        drop(lock);
        assert!(!super::status(&home).unwrap().running);
        assert!(!super::runtime_path(&home).exists());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn managed_proxy_requires_exact_local_endpoint() {
        assert!(super::should_manage(Some("http://127.0.0.1:18080")));
        assert!(super::should_manage(Some("http://localhost:18080")));
        assert!(!super::should_manage(Some(
            "http://example.com/127.0.0.1:18080"
        )));
        assert!(!super::should_manage(Some("http://127.0.0.1:18081")));
        assert!(!super::should_manage(Some("https://127.0.0.1:18080")));
    }

    #[test]
    fn managed_network_has_only_a_restricted_proxy_forward() {
        let mut cfg = crate::config::Config {
            proxy: Some("http://127.0.0.1:18080".into()),
            ..crate::config::Config::default()
        };
        let net = super::user_netdev(&cfg, "mgmt").unwrap();
        assert!(net.contains("restrict=on"));
        assert!(net.contains("ipv6=off"));
        assert!(net.contains("guestfwd=tcp:10.0.2.2:18080"));
        cfg.proxy = None;
        assert!(
            !super::user_netdev(&cfg, "mgmt")
                .unwrap()
                .contains("guestfwd=")
        );
    }

    #[test]
    fn global_probe_errors_are_actionable() {
        assert_eq!(
            super::global_probe_failure(56, "curl: (56) Proxy CONNECT aborted"),
            "The proxy closed the connection before the test finished."
        );
        assert_eq!(
            super::global_probe_failure(7, "curl: (7) Failed to connect"),
            "The local proxy listener is unreachable."
        );
        assert_eq!(
            super::global_probe_failure(28, "Operation timed out"),
            "Connection timed out"
        );
    }
}
