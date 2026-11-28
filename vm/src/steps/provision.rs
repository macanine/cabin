//! First-boot provisioning: boot cloud image, wait for cloud-init, verify, seal.

use std::collections::BTreeMap;

use base64::Engine;
use serde_json::{Map, Value, json};

use crate::Result;
use crate::proxy;
use crate::steps::base::{CheckResult, Step, StepContext, ensure_stopped};
use crate::util::{atomic_write_json, ensure_dir, message, now_millis, read_json, tail_file};
use crate::vm::{Boot, Runtime, resolve_qemu};

fn sections(output: &str) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut buffer: Vec<String> = Vec::new();
    for line in output.lines() {
        if line.starts_with("__VMCTL_") && line.ends_with("__") {
            if let Some(name) = current.take() {
                result.insert(name, buffer.join("\n").trim().to_string());
            }
            current = Some(line[8..line.len() - 2].to_string());
            buffer = Vec::new();
        } else if current.is_some() {
            buffer.push(line.to_string());
        }
    }
    if let Some(name) = current {
        result.insert(name, buffer.join("\n").trim().to_string());
    }
    result
}

fn parse_ips(raw: &str) -> Map<String, Value> {
    let mut ips = Map::new();
    for line in raw.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 4 && parts[2] == "inet" {
            ips.insert(
                parts[1].to_string(),
                json!(parts[3].split('/').next().unwrap_or("")),
            );
        }
    }
    ips
}

const VERIFY_SCRIPT: &str = r#"
set +e
echo __VMCTL_HOSTNAME__
hostname
echo __VMCTL_USER__
id -un
echo __VMCTL_CLOUD_INIT__
cloud-init status --long
echo __VMCTL_QGA__
systemctl is-active qemu-guest-agent
echo __VMCTL_IPS__
ip -o -4 addr show
echo __VMCTL_PROXY__
cat /etc/vmctl-proxy 2>/dev/null
echo __VMCTL_APT_PROXY__
apt-config dump 2>/dev/null | grep -i 'Acquire::.*Proxy'
echo __VMCTL_PACKAGE__
dpkg-query -W -f='${Status}' qemu-guest-agent 2>/dev/null
echo
echo __VMCTL_SUDO__
sudo -n true && echo yes || echo no
"#;

/// The verification script, with the kernel package checks for releases
/// that switch kernels (`os::Os::kernel`).
fn verify_script(kernel: crate::os::KernelSwitch) -> String {
    let mut script = VERIFY_SCRIPT.to_string();
    script.push_str(&format!(
        r#"echo __VMCTL_KERNEL_GENERIC__
dpkg-query -W -f='${{db:Status-Status}}' {install} 2>/dev/null
echo
echo __VMCTL_KERNEL_MANUAL__
apt-mark showmanual {install} 2>/dev/null
echo __VMCTL_KERNEL_VIRTUAL__
for package in {replaces}; do
    if [ "$(dpkg-query -W -f='${{db:Status-Status}}' "$package" 2>/dev/null)" = installed ]; then
        echo "$package"
    fi
done
"#,
        install = kernel.install,
        replaces = kernel.replaces.join(" "),
    ));
    script.push_str("echo __VMCTL_END__\n");
    script
}

fn verify_kernel_packages(
    kernel: crate::os::KernelSwitch,
    data: &BTreeMap<String, String>,
) -> Result<()> {
    if data.get("KERNEL_GENERIC").map(String::as_str) != Some("installed") {
        return message(format!(
            "{} is not installed; kernel metapackage replacement failed",
            kernel.install
        ));
    }
    if data.get("KERNEL_MANUAL").map(String::as_str) != Some(kernel.install) {
        return message(format!(
            "{} is not marked as manually installed",
            kernel.install
        ));
    }
    match data.get("KERNEL_VIRTUAL") {
        Some(packages) if packages.is_empty() => {}
        Some(packages) => {
            return message(format!(
                "virtual kernel metapackages are still installed: {packages}"
            ));
        }
        None => return message("could not verify whether virtual kernel metapackages remain"),
    }
    Ok(())
}

const CLOUD_INIT_LOG: &str = "/var/log/cloud-init-output.log";
/// Upper bound on log bytes read per poll, so a burst cannot stall a poll.
const CLOUD_INIT_CHUNK: u64 = 256 * 1024;

/// One poll: the log bytes after `offset`, the log's size, and the status.
/// The log is root:adm 0640, so read it through sudo when that works.
fn cloud_init_poll_command(offset: u64) -> String {
    format!(
        r#"f={CLOUD_INIT_LOG}; r() {{ sudo -n "$@" 2>/dev/null || "$@" 2>/dev/null; }}
s=$(r stat -c %s "$f" || echo 0)
if [ "$s" -gt {offset} ]; then n=$((s - {offset})); [ "$n" -gt {CLOUD_INIT_CHUNK} ] && n={CLOUD_INIT_CHUNK}; r tail -c +{start} "$f" | head -c "$n"; s=$(({offset} + n)); fi
echo; echo __VMCTL_SIZE__; echo "$s"; echo __VMCTL_STATUS__; cloud-init status 2>/dev/null"#,
        start = offset + 1,
    )
}

/// Splits one poll's output into (log chunk, new offset, status line).
fn parse_cloud_init_poll(output: &str) -> Option<(&str, u64, &str)> {
    let (chunk, rest) = output.rsplit_once("\n__VMCTL_SIZE__\n")?;
    let (size, status) = rest.split_once("\n__VMCTL_STATUS__\n")?;
    Some((chunk, size.trim().parse().ok()?, status.trim()))
}

fn cloud_init_still_running(status: &str) -> bool {
    status.is_empty()
        || ["running", "not started", "not run"]
            .iter()
            .any(|s| status.contains(s))
}

/// Streams the guest's cloud-init output into the step log until cloud-init
/// leaves the running state or `timeout` passes. Complete lines are logged as
/// they arrive; failed polls (sshd restarting, sudo not ready) are retried.
fn follow_cloud_init(ctx: &StepContext, runtime: &Runtime, timeout: std::time::Duration) {
    let deadline = std::time::Instant::now() + timeout;
    let mut offset = 0u64;
    let mut pending = String::new();
    while std::time::Instant::now() < deadline {
        let (rc, out) = runtime.ssh_capture(
            &cloud_init_poll_command(offset),
            std::time::Duration::from_secs(30),
        );
        if let Some((chunk, size, status)) = parse_cloud_init_poll(&out).filter(|_| rc != 255) {
            pending.push_str(chunk);
            while let Some(end) = pending.find('\n') {
                let line: String = pending.drain(..=end).collect();
                let line = line.trim_end();
                if !line.is_empty() {
                    ctx.log(&format!("cloud-init | {line}"));
                }
            }
            offset = size;
            if !cloud_init_still_running(status) {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    if !pending.trim().is_empty() {
        ctx.log(&format!("cloud-init | {}", pending.trim_end()));
    }
}

pub struct ProvisionStep;

impl ProvisionStep {
    fn verify(&self, ctx: &mut StepContext, runtime: &Runtime) -> Result<Value> {
        let cfg = ctx.cfg.clone();
        ctx.log("waiting for cloud-init to finish (streaming /var/log/cloud-init-output.log)");
        follow_cloud_init(
            ctx,
            runtime,
            std::time::Duration::from_secs(cfg.provision_timeout),
        );
        let (rc, out) = runtime.ssh_capture(
            "cloud-init status --wait --long",
            std::time::Duration::from_secs(120),
        );
        if !out.is_empty() {
            ctx.log(&out);
        }
        if rc != 0 || !out.contains("status: done") {
            let (_, diag) = runtime.ssh_capture(
                "sudo -n sh -c 'cloud-init status --long; echo ---; tail -n 120 /var/log/cloud-init-output.log' 2>&1",
                std::time::Duration::from_secs(60),
            );
            if !diag.is_empty() {
                ctx.log(&format!("cloud-init diagnostics:\n{diag}"));
            }
            return message(format!("cloud-init did not finish cleanly (rc={rc})"));
        }
        ctx.log("running in-guest acceptance checks");
        let (rc, out) = runtime.ssh_capture(
            &verify_script(cfg.guest_os().kernel()),
            std::time::Duration::from_secs(120),
        );
        ctx.log(&out);
        if rc != 0 {
            return message(format!("acceptance script failed (rc={rc})"));
        }
        let data = sections(&out);
        if data.get("HOSTNAME").map(String::as_str) != Some(cfg.hostname.as_str()) {
            return message(format!(
                "hostname is {:?}, expected {:?}",
                data.get("HOSTNAME"),
                cfg.hostname
            ));
        }
        if data.get("USER").map(String::as_str) != Some(cfg.username.as_str()) {
            return message(format!(
                "user is {:?}, expected {:?}",
                data.get("USER"),
                cfg.username
            ));
        }
        if !data
            .get("CLOUD_INIT")
            .map(|value| value.contains("status: done"))
            .unwrap_or(false)
        {
            return message("cloud-init status is not done");
        }
        if !cfg.device_enabled() {
            if data.get("QGA").map(String::as_str) != Some("active") {
                return message(format!(
                    "qemu-guest-agent is not active: {:?}",
                    data.get("QGA")
                ));
            }
            if !data
                .get("PACKAGE")
                .map(|value| value.contains("install ok installed"))
                .unwrap_or(false)
            {
                return message("qemu-guest-agent package is not installed");
            }
        }
        if data.get("SUDO").map(String::as_str) != Some("yes") {
            return message("passwordless sudo for the provisioned user is not working");
        }
        verify_kernel_packages(cfg.guest_os().kernel(), &data)?;
        if let Some(guest_proxy) = cfg.guest_proxy_url() {
            let proxy_file = data.get("PROXY").map(|value| value.trim()).unwrap_or("");
            if !cfg.device_enabled() && proxy_file != guest_proxy {
                return message(format!(
                    "guest proxy marker mismatch: {proxy_file:?}, expected {guest_proxy:?}"
                ));
            }
            if !data
                .get("APT_PROXY")
                .map(|value| value.contains(&guest_proxy))
                .unwrap_or(false)
            {
                return message("apt does not report the configured proxy");
            }
        }
        let mut ips = parse_ips(data.get("IPS").map(String::as_str).unwrap_or(""));
        let mut expected_iface = if cfg.isolated() { "data0" } else { "mgmt0" }.to_string();
        if cfg.device_enabled() && !cfg.isolated() {
            let resolved = cfg.device_resolved()?;
            expected_iface = resolved
                .get("network")
                .and_then(|value| value.get("interface"))
                .and_then(Value::as_str)
                .unwrap_or("enp0s2")
                .to_string();
        }
        if !ips.contains_key(&expected_iface) {
            return message(format!("NIC {expected_iface} has no IPv4 address: {ips:?}"));
        }
        if cfg.device_enabled() && !cfg.isolated() {
            let resolved = cfg.device_resolved()?;
            let network = resolved.get("network").unwrap_or(&Value::Null);
            if let Some(expected_address) = network
                .get("address")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                let expected_ip = expected_address.split('/').next().unwrap_or("");
                if ips.get(&expected_iface).and_then(Value::as_str) != Some(expected_ip) {
                    return message(format!(
                        "{expected_iface} address is {:?}, expected {expected_ip:?}",
                        ips.get(&expected_iface)
                    ));
                }
            } else if network
                .get("require_bridged")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                for (name, address) in &ips {
                    if name != "lo"
                        && address
                            .as_str()
                            .map(|value| value.starts_with("10.0.2."))
                            .unwrap_or(false)
                    {
                        return message(format!(
                            "{name} received a slirp address {address}; the device profile requires bridged networking"
                        ));
                    }
                }
            }
        }
        if cfg.isolated() || (cfg.needs_second_nic() && !cfg.device_enabled()) {
            ctx.log("waiting for the data0 NIC to receive a DHCP lease");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
            loop {
                let current = runtime.guest_ips();
                if let Some(address) = current.get("data0")
                    && !address.starts_with("10.0.2.")
                {
                    ips.remove("data0");
                    ips.insert("data0".to_string(), json!(address));
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    return message(format!(
                        "data0 did not get a DHCP lease; interfaces={:?} (network mode {}, iface {})",
                        runtime.guest_ips(),
                        cfg.network_mode,
                        cfg.bridge_if
                    ));
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
            ctx.log(&format!(
                "data0 address: {}",
                ips.get("data0").and_then(Value::as_str).unwrap_or("")
            ));
        }
        let mut qga = false;
        if !cfg.device_enabled() {
            for _ in 0..10 {
                if runtime.qga_ping() {
                    qga = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            if !qga {
                ctx.log("warning: QMP guest-agent ping did not respond (SSH checks passed)");
            }
        }
        Ok(json!({"ips": Value::Object(ips), "qga": qga, "cloud_init": "done"}))
    }

    fn wait_stopped(&self, ctx: &StepContext, runtime: &Runtime) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while std::time::Instant::now() < deadline && runtime.alive() {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        if runtime.alive() {
            ctx.log("guest did not power off in time; forcing QEMU stop");
            runtime.stop(&ctx.log_file, std::time::Duration::from_secs(30));
        }
        runtime.cleanup_stale();
    }
    fn enforce_proxy_lock(&self, ctx: &mut StepContext, runtime: &Runtime) -> Result<()> {
        let cfg = &ctx.cfg;
        if !proxy::should_manage(cfg.proxy.as_deref()) {
            return Ok(());
        }
        let script = r#"#!/bin/sh
set -eu
export DEBIAN_FRONTEND=noninteractive
dpkg --configure -a --force-confdef --force-confold || true
apt-get install -y --no-install-recommends iptables
cat > /usr/local/sbin/vmctl-proxy-lock <<'LOCK'
#!/bin/sh
set -eu
iptables -F OUTPUT
iptables -A OUTPUT -o lo -j ACCEPT
iptables -A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
iptables -A OUTPUT -d 10.0.2.2 -p tcp --dport 18080 -j ACCEPT
iptables -A OUTPUT -d 10.0.2.3 -p udp --dport 53 -j ACCEPT
iptables -A OUTPUT -d 10.0.2.3 -p tcp --dport 53 -j ACCEPT
iptables -A OUTPUT -j REJECT
ip6tables -F OUTPUT
ip6tables -A OUTPUT -o lo -j ACCEPT
ip6tables -A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
ip6tables -A OUTPUT -j REJECT
LOCK
chmod 0755 /usr/local/sbin/vmctl-proxy-lock
cat > /etc/systemd/system/vmctl-proxy-lock.service <<'UNIT'
[Unit]
Description=vmctl deny direct egress outside the embedded proxy
After=network-online.target
Wants=network-online.target
Before=ssh.service

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/vmctl-proxy-lock
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable vmctl-proxy-lock.service
nohup sh -c 'sleep 3; systemctl start vmctl-proxy-lock.service' >/dev/null 2>&1 </dev/null &
"#;
        let encoded = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
        let command = format!(
            "printf '%s' {encoded} | base64 -d | sudo -n tee /tmp/vmctl-proxy-lock.sh >/dev/null && sudo -n chmod 0700 /tmp/vmctl-proxy-lock.sh && sudo -n /tmp/vmctl-proxy-lock.sh"
        );
        ctx.log("installing guest egress lock: only the embedded proxy may reach the network");
        let (rc, output) = runtime.ssh_capture(&command, std::time::Duration::from_secs(180));
        if rc != 0 {
            return message(format!(
                "proxy egress lock failed (rc={rc}): {}",
                output.trim()
            ));
        }
        Ok(())
    }
}

impl Step for ProvisionStep {
    fn id(&self) -> &str {
        "provision"
    }
    fn title(&self) -> &str {
        "Boot, run cloud-init, verify user/hostname/network/proxy"
    }

    fn check(&self, ctx: &mut StepContext) -> Result<CheckResult> {
        let cfg = ctx.cfg.clone();
        if !cfg.disk_path().exists() || !cfg.seed_path().exists() {
            return Ok(CheckResult::pending("disk or seed is missing"));
        }
        if !cfg.provision_marker_path().exists() {
            return Ok(CheckResult::pending("provisioned.json marker is missing"));
        }
        let marker = read_json(&cfg.provision_marker_path()).unwrap_or(Value::Null);
        if marker.get("hostname").and_then(Value::as_str) != Some(cfg.hostname.as_str())
            || marker.get("username").and_then(Value::as_str) != Some(cfg.username.as_str())
        {
            return Ok(CheckResult::pending(
                "provisioned marker does not match this configuration",
            ));
        }
        if cfg.device_enabled()
            && marker.get("device_fingerprint").and_then(Value::as_str)
                != Some(cfg.device_fingerprint(None).unwrap_or_default().as_str())
        {
            return Ok(CheckResult::pending(
                "device profile changed after provisioning",
            ));
        }
        if proxy::should_manage(cfg.proxy.as_deref())
            && marker.get("proxy_lock").and_then(Value::as_bool) != Some(true)
        {
            return Ok(CheckResult::pending("embedded proxy lock is missing"));
        }
        let ips = marker.get("ips").cloned().unwrap_or(json!({}));
        Ok(CheckResult::ready(format!(
            "provisioned, ips={}",
            crate::py_json::py_repr(&ips)
        )))
    }

    fn apply(&self, ctx: &mut StepContext) -> Result<()> {
        let cfg = ctx.cfg.clone();
        ensure_dir(&cfg.instance_dir())?;
        let runtime = Runtime::new(&cfg);
        ensure_stopped(&cfg, ctx)?;
        if !cfg.disk_path().exists() {
            return message("disk is missing");
        }
        if !cfg.seed_path().exists() {
            return message("seed ISO is missing");
        }
        let _ = std::fs::remove_file(cfg.provision_serial_log());
        for socket in [cfg.qmp_socket(), cfg.qga_socket()] {
            let _ = std::fs::remove_file(socket);
        }
        let _ = std::fs::remove_file(cfg.pidfile());
        let _ = std::fs::remove_file(cfg.provision_marker_path());
        if proxy::should_manage(cfg.proxy.as_deref()) {
            ctx.log("starting embedded shoes proxy before guest provisioning");
            proxy::ensure_started(&cfg.home)?;
        }
        ctx.log(&format!(
            "booting {} ARM64 for first-boot provisioning (network={}, cpus={}, memory={}MiB)",
            cfg.guest_os().name,
            cfg.network_mode,
            cfg.cpus,
            cfg.memory_mb
        ));
        let started = std::time::Instant::now();
        let result = (|| -> Result<()> {
            runtime.start(
                &ctx.log_file,
                Boot {
                    // Emulation without HVF/KVM takes longer to reach SSH.
                    ssh_timeout: std::time::Duration::from_secs(
                        cfg.provision_timeout
                            .min(300 * u64::from(crate::platform::slowdown())),
                    ),
                    serial_log: Some(&cfg.provision_serial_log()),
                    phase: cfg.device_enabled().then_some("install"),
                    display_mode: Some("none"),
                },
            )?;
            let verification = self.verify(ctx, &runtime)?;
            self.enforce_proxy_lock(ctx, &runtime)?;
            ctx.log("cloud-init complete; shutting down cleanly");
            runtime.ssh_capture(
                "sudo -n shutdown -P now",
                std::time::Duration::from_secs(20),
            );
            self.wait_stopped(ctx, &runtime);
            let marker = json!({
                "provisioned": true,
                "name": cfg.name,
                "hostname": cfg.hostname,
                "username": cfg.username,
                "network_mode": cfg.network_mode,
                "ssh_port": cfg.ssh_port,
                "ips": verification.get("ips").cloned().unwrap_or(Value::Null),
                "qga": verification.get("qga").cloned().unwrap_or(json!(false)),
                "cloud_init": verification.get("cloud_init").cloned().unwrap_or(json!("done")),
                "device": cfg.device_enabled(),
                "device_fingerprint": cfg.device_fingerprint(None).unwrap_or_default(),
                "runtime_phase": if cfg.device_enabled() { "install" } else { "legacy" },
                "proxy_lock": proxy::should_manage(cfg.proxy.as_deref()),
                "duration_s": ((started.elapsed().as_secs_f64() * 10.0).round() / 10.0),
                "provisioned_at": now_millis(),
            });
            atomic_write_json(&cfg.provision_marker_path(), &marker, 0o600)?;
            ctx.track_file(&cfg.provision_marker_path())?;
            ctx.set_meta("provisioned", json!(true));
            ctx.set_meta("ips", marker.get("ips").cloned().unwrap_or(Value::Null));
            ctx.set_meta("qga", marker.get("qga").cloned().unwrap_or(json!(false)));
            ctx.set_meta(
                "duration_s",
                marker.get("duration_s").cloned().unwrap_or(json!(0)),
            );
            let duration = marker
                .get("duration_s")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let ips = marker.get("ips").cloned().unwrap_or(Value::Null);
            ctx.log(&format!("provisioning complete in {duration}s; ips={ips}"));
            Ok(())
        })();
        if result.is_err() {
            let serial_tail = tail_file(&cfg.provision_serial_log(), 80);
            if !serial_tail.is_empty() {
                ctx.log(&format!("serial log tail:\n{serial_tail}"));
            }
        }
        result
    }

    fn rollback(&self, ctx: &mut StepContext, _state: &Value, force: bool) -> Result<()> {
        let cfg = ctx.cfg.clone();
        if !force && cfg.disk_path().exists() {
            return message(
                "Provisioning rollback would erase the existing disk. The disk was kept.",
            );
        }
        ensure_stopped(&cfg, ctx)?;
        if cfg.identity_path().exists() {
            ctx.log(&format!(
                "remove {} (provision rollback resets the guest identity)",
                cfg.identity_path().display()
            ));
            let _ = std::fs::remove_file(cfg.identity_path());
        }
        // First boot cannot be undone in place: drop the disk, so the disk
        // step makes a fresh copy of the base image before the next attempt.
        if cfg.disk_path().exists() {
            ctx.log(&format!(
                "remove {} (a fresh disk is copied before provisioning again)",
                cfg.disk_path().display()
            ));
            std::fs::remove_file(cfg.disk_path())?;
        }
        for path in [cfg.provision_marker_path(), cfg.runtime_path()] {
            if path.exists() {
                ctx.log(&format!("remove {}", path.display()));
                let _ = std::fs::remove_file(path);
            }
        }
        for path in [cfg.qmp_socket(), cfg.qga_socket(), cfg.pidfile()] {
            let _ = std::fs::remove_file(path);
        }
        if cfg.efi_vars_path().exists()
            && let Ok(resolved) = resolve_qemu(&cfg)
        {
            let tmp = cfg.efi_vars_path().with_file_name(".efi-vars.rollback.tmp");
            let copied = (|| -> Result<()> {
                std::fs::copy(&resolved.firmware_vars, &tmp)?;
                crate::util::chmod(&tmp, 0o600)?;
                std::fs::rename(&tmp, cfg.efi_vars_path())?;
                Ok(())
            })();
            match copied {
                Ok(()) => ctx.log("UEFI variable store reset to the firmware template"),
                Err(error) => ctx.log(&format!("warning: could not reset UEFI vars: {error}")),
            }
        }
        ctx.log("provision rollback complete; the next apply starts from a fresh disk");
        Ok(())
    }
}

#[cfg(test)]
mod kernel_tests {
    use super::*;

    #[test]
    fn provisioning_requires_a_manual_generic_metapackage_and_no_virtual_metapackages() {
        let ubuntu = crate::os::find("ubuntu-24.04").unwrap().kernel();
        let mut data = BTreeMap::from([
            ("KERNEL_GENERIC".to_string(), "installed".to_string()),
            (
                "KERNEL_MANUAL".to_string(),
                "linux-image-generic".to_string(),
            ),
            ("KERNEL_VIRTUAL".to_string(), String::new()),
        ]);
        assert!(verify_kernel_packages(ubuntu, &data).is_ok());
        data.insert(
            "KERNEL_VIRTUAL".to_string(),
            "linux-image-virtual".to_string(),
        );
        assert!(verify_kernel_packages(ubuntu, &data).is_err());
        data.remove("KERNEL_VIRTUAL");
        assert!(verify_kernel_packages(ubuntu, &data).is_err());
        data.insert("KERNEL_VIRTUAL".to_string(), String::new());
        data.insert("KERNEL_MANUAL".to_string(), String::new());
        assert!(verify_kernel_packages(ubuntu, &data).is_err());
        data.insert(
            "KERNEL_MANUAL".to_string(),
            "linux-image-generic".to_string(),
        );
        data.remove("KERNEL_GENERIC");
        assert!(verify_kernel_packages(ubuntu, &data).is_err());
    }

    #[test]
    fn verify_script_checks_the_generic_kernel() {
        let script = verify_script(crate::os::find("ubuntu-26.04").unwrap().kernel());
        assert!(script.contains("apt-mark showmanual linux-image-generic"));
        assert!(script.contains("${db:Status-Status}"));
        assert!(script.ends_with("echo __VMCTL_END__\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_init_poll_keeps_partial_lines_and_reads_status() {
        let out =
            "Get:1 http://ports\npartial\n__VMCTL_SIZE__\n42\n__VMCTL_STATUS__\nstatus: running\n";
        assert_eq!(
            parse_cloud_init_poll(out),
            Some(("Get:1 http://ports\npartial", 42, "status: running"))
        );
        let empty = "\n__VMCTL_SIZE__\n0\n__VMCTL_STATUS__\n";
        assert_eq!(parse_cloud_init_poll(empty), Some(("", 0, "")));
        assert_eq!(parse_cloud_init_poll("ssh: connect refused"), None);
        for (status, running) in [
            ("", true),
            ("status: not started", true),
            ("status: running", true),
            ("status: done", false),
            ("status: error", false),
        ] {
            assert_eq!(cloud_init_still_running(status), running, "{status}");
        }
    }
}
