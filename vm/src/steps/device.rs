//! Atomic device steps and the post-provision seal/verification step.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Result;
use crate::config::Config;
use crate::device::guest as device_guest;
use crate::device::profile as sprofile;
use crate::device::render as device_render;
use crate::device::smbios as device_smbios;
use crate::device::verify as device_verify;
use crate::journal::load_state;
use crate::steps::base::{CheckResult, Step, StepContext, delete_artifacts, ensure_stopped};
use crate::util::{
    atomic_write_bytes, atomic_write_json, capture, ensure_dir, message, now_millis, read_json,
    sha256_file, sha256_text, tail_file, utcnow,
};
use crate::vm::{Boot, Runtime, resolve_qemu};

fn resolved(cfg: &Config) -> Result<Value> {
    if !cfg.device_enabled() {
        return message("device is not enabled for this instance");
    }
    cfg.device_resolved()
}

#[cfg(all(test, target_os = "macos"))]
mod network_tests {
    use super::*;

    #[test]
    #[ignore = "requires a local QEMU installation"]
    fn gateway_network_step_accepts_forwarded_ssh() {
        let cfg = crate::config::build_config(
            &json!({"network_mode": "gateway", "device": {"enabled": true}}),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .unwrap();
        DeviceGroupStep::new("network", "network.json")
            .validate(&cfg)
            .unwrap();
    }
}

fn qemu_device_help(cfg: &Config) -> Result<String> {
    let resolved = resolve_qemu(cfg)?;
    let result = capture(
        &[resolved.qemu_bin, "-device".to_string(), "help".to_string()],
        std::time::Duration::from_secs(20),
        None,
    );
    if result.code != 0 {
        return message(format!(
            "qemu -device help failed: {}",
            result.output.trim()
        ));
    }
    Ok(result.output)
}

fn qemu_dry_run(cfg: &Config, fragment: &[String]) -> (i32, String) {
    let resolved = match resolve_qemu(cfg) {
        Ok(resolved) => resolved,
        Err(error) => return (125, error.to_string()),
    };
    let mut argv = vec![
        resolved.qemu_bin,
        "-machine".to_string(),
        format!("virt,accel={}", crate::platform::accel().accel),
        "-display".to_string(),
        "none".to_string(),
        "-monitor".to_string(),
        "none".to_string(),
        "-nodefaults".to_string(),
        "-S".to_string(),
    ];
    argv.extend(fragment.iter().cloned());
    let result = capture(&argv, std::time::Duration::from_millis(2500), None);
    (result.code, result.output)
}

fn require_dry_run(cfg: &Config, fragment: &[String], what: &str) -> Result<()> {
    let (rc, out) = qemu_dry_run(cfg, fragment);
    // 124 means QEMU started and was killed by the timeout: the options parsed.
    if rc != 124 {
        return message(format!(
            "QEMU rejected the {what} arguments (rc={rc}):\n{}",
            out.trim()
        ));
    }
    Ok(())
}

fn device_probe_fragment(resolved: &Value, phase: &str) -> Vec<String> {
    let devices = resolved.get("devices").unwrap_or(&Value::Null);
    let storage = devices.get("storage").unwrap_or(&Value::Null);
    let mut fragment = Vec::new();
    if phase == "runtime" {
        fragment.push("-drive".to_string());
        fragment.push("if=none,id=probe,file=null-co://,format=raw".to_string());
        let storage_controller = storage
            .get("controller")
            .and_then(Value::as_str)
            .unwrap_or("nvme");
        if storage_controller == "nvme" {
            fragment.push("-device".to_string());
            fragment.push("nvme,id=nvme0,serial=PROBE123,model=PROBE NVME,firmware-version=PROBE,use-intel-id=on".to_string());
            fragment.push("-device".to_string());
            fragment.push("nvme-ns,drive=probe,bus=nvme0".to_string());
        } else {
            fragment.push("-device".to_string());
            fragment.push("ich9-ahci,id=sata".to_string());
            fragment.push("-device".to_string());
            fragment
                .push("ide-hd,drive=probe,bus=sata.0,model=PROBE SSD,serial=PROBE123".to_string());
        }
        let optical = devices.get("optical").unwrap_or(&Value::Null);
        if optical
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            if optical
                .get("controller")
                .and_then(Value::as_str)
                .unwrap_or("lsi53c895a")
                == "lsi53c895a"
            {
                fragment.push("-device".to_string());
                fragment.push("lsi53c895a,id=scsi".to_string());
                fragment.push("-device".to_string());
                fragment.push("scsi-cd,bus=scsi.0,scsi-id=0,lun=0,vendor=PROBE,product=DVD,serial=PROBE123,ver=1.00".to_string());
            } else {
                if storage_controller != "ahci" {
                    fragment.push("-device".to_string());
                    fragment.push("ich9-ahci,id=sata".to_string());
                }
                fragment.push("-device".to_string());
                fragment.push("ide-cd,bus=sata.1,model=PROBE DVD,serial=PROBE123".to_string());
            }
        }
    } else {
        fragment.push("-drive".to_string());
        fragment.push("if=none,id=probe,file=null-co://,format=raw".to_string());
        fragment.push("-device".to_string());
        fragment.push("virtio-blk-pci,drive=probe".to_string());
    }
    let usb = devices.get("usb").unwrap_or(&Value::Null);
    if usb.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
        fragment.push("-device".to_string());
        fragment.push(format!(
            "{},id=usb",
            usb.get("controller")
                .and_then(Value::as_str)
                .unwrap_or("nec-usb-xhci")
        ));
    }
    let sound = devices.get("sound").unwrap_or(&Value::Null);
    if sound
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        fragment.push("-audiodev".to_string());
        fragment.push("none,id=audio0".to_string());
        if sound.get("controller").and_then(Value::as_str) == Some("AC97") {
            fragment.push("-device".to_string());
            fragment.push("AC97,audiodev=audio0".to_string());
        } else {
            fragment.push("-device".to_string());
            fragment.push(format!(
                "{},id=snd",
                sound
                    .get("controller")
                    .and_then(Value::as_str)
                    .unwrap_or("ich9-intel-hda")
            ));
            fragment.push("-device".to_string());
            fragment.push(format!(
                "{},bus=snd.0,cad=0,audiodev=audio0",
                sound
                    .get("codec")
                    .and_then(Value::as_str)
                    .unwrap_or("hda-duplex")
            ));
        }
    }
    fragment.push("-netdev".to_string());
    fragment.push("user,id=probe0".to_string());
    fragment.push("-device".to_string());
    fragment.push(format!(
        "{},netdev=probe0,mac=00:1b:21:00:00:01",
        resolved
            .get("network")
            .and_then(|value| value.get("nic_model"))
            .and_then(Value::as_str)
            .unwrap_or("e1000e")
    ));
    fragment
}

pub struct DeviceGroupStep {
    pub group: &'static str,
    pub artifact_name: &'static str,
    pub title: String,
}

impl DeviceGroupStep {
    pub fn new(group: &'static str, artifact_name: &'static str) -> Self {
        Self {
            group,
            artifact_name,
            title: format!("Device {group}: render and validate"),
        }
    }

    pub fn artifact(&self, cfg: &Config) -> PathBuf {
        cfg.device_dir().join(self.artifact_name)
    }

    fn validate(&self, cfg: &Config) -> Result<()> {
        sprofile::validate(&cfg.device, &cfg.name, cfg.cpus, cfg.memory_mb)?;
        let resolved = resolved(cfg)?;
        match self.group {
            "cpu" => {
                require_dry_run(cfg, &device_render::cpu_args(&resolved), "CPU")?;
            }
            "smbios" => {
                let data = device_smbios::build(&resolved, &cfg.name, cfg.cpus, cfg.memory_mb)?;
                device_smbios::validate_table(&data)?;
            }
            "devices" => {
                let help = qemu_device_help(cfg)?;
                let devices = resolved.get("devices").unwrap_or(&Value::Null);
                let storage = devices.get("storage").unwrap_or(&Value::Null);
                let mut required = vec![
                    storage
                        .get("controller")
                        .and_then(Value::as_str)
                        .unwrap_or("nvme")
                        .to_string(),
                    devices
                        .get("usb")
                        .and_then(|value| value.get("controller"))
                        .and_then(Value::as_str)
                        .unwrap_or("nec-usb-xhci")
                        .to_string(),
                ];
                let optical = devices.get("optical").unwrap_or(&Value::Null);
                if optical
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                {
                    if optical
                        .get("controller")
                        .and_then(Value::as_str)
                        .unwrap_or("lsi53c895a")
                        == "lsi53c895a"
                    {
                        required.push("lsi53c895a".to_string());
                        required.push("scsi-cd".to_string());
                    } else {
                        required.push("ich9-ahci".to_string());
                        required.push("ide-cd".to_string());
                    }
                }
                if let Some(sound) = devices.get("sound")
                    && sound
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(true)
                {
                    required.push(
                        sound
                            .get("controller")
                            .and_then(Value::as_str)
                            .unwrap_or("ich9-intel-hda")
                            .to_string(),
                    );
                    required.push(
                        sound
                            .get("codec")
                            .and_then(Value::as_str)
                            .unwrap_or("hda-duplex")
                            .to_string(),
                    );
                }
                let missing: Vec<&String> = required
                    .iter()
                    .filter(|name| !help.contains(&format!("name \"{name}\"")))
                    .collect();
                if !missing.is_empty() {
                    return message(format!(
                        "QEMU does not support required device models: {missing:?}"
                    ));
                }
                require_dry_run(
                    cfg,
                    &device_probe_fragment(&resolved, "install"),
                    "install devices",
                )?;
                require_dry_run(
                    cfg,
                    &device_probe_fragment(&resolved, "runtime"),
                    "runtime devices",
                )?;
            }
            "network" => {
                let network = resolved.get("network").unwrap_or(&Value::Null);
                let mode = network
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("user");
                if network
                    .get("require_bridged")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    && mode == "user"
                {
                    return message(
                        "device.network.require_bridged is true but the network mode is 'user'; use a bridged socket_vmnet daemon or set require_bridged=false for a lab VM",
                    );
                }
                if mode == "socket-vmnet" {
                    if !Path::new(&cfg.socket_vmnet_client).is_file() {
                        return message(format!(
                            "socket_vmnet_client is missing: {}",
                            cfg.socket_vmnet_client
                        ));
                    }
                    if !Path::new(&cfg.socket_vmnet_socket).exists() {
                        return message(format!(
                            "socket_vmnet socket is missing: {}",
                            cfg.socket_vmnet_socket
                        ));
                    }
                }
                // Use the same endpoint rules as the actual SSH connection.
                device_render::ssh_endpoint(cfg, Some(&resolved))?;
                if mode != "socket-vmnet" {
                    require_dry_run(
                        cfg,
                        &[
                            "-netdev".to_string(),
                            "user,id=probe0".to_string(),
                            "-device".to_string(),
                            format!(
                                "{},netdev=probe0,mac={}",
                                network
                                    .get("nic_model")
                                    .and_then(Value::as_str)
                                    .unwrap_or("e1000e"),
                                network.get("mac").and_then(Value::as_str).unwrap_or("")
                            ),
                        ],
                        "network device",
                    )?;
                }
            }
            "guest" => {
                device_guest::cloud_init_packages(&resolved);
                device_guest::cloud_init_write_files(cfg, &resolved)?;
                device_guest::cloud_init_runcmd(cfg, &resolved);
                device_guest::alignment_script(cfg, &resolved)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn render(&self, cfg: &Config) -> Result<Value> {
        let resolved = resolved(cfg)?;
        let fingerprint = cfg.device_fingerprint(Some(self.group))?;
        Ok(match self.group {
            "cpu" => json!({
                "group": self.group,
                "fingerprint": fingerprint,
                "args": device_render::cpu_args(&resolved),
                "cpu": resolved.get("cpu").cloned().unwrap_or(Value::Null),
                "residuals": [
                    "HVF keeps the host MIDR feature registers; QEMU only exposes midr/pauth/pmu/SME as settable host-CPU properties."
                ],
            }),
            "devices" => json!({
                "group": self.group,
                "fingerprint": fingerprint,
                "install_args": device_render::device_args(cfg, &resolved, "install")?,
                "runtime_args": device_render::device_args(cfg, &resolved, "runtime")?,
                "devices": resolved.get("devices").cloned().unwrap_or(Value::Null),
            }),
            "network" => {
                let (host, port) = device_render::ssh_endpoint(cfg, Some(&resolved))?;
                json!({
                    "group": self.group,
                    "fingerprint": fingerprint,
                    "args": device_render::network_args(cfg, &resolved)?,
                    "network": resolved.get("network").cloned().unwrap_or(Value::Null),
                    "plan": device_render::netplan_document(&resolved, Some(&cfg.hostname))?,
                    "ssh_endpoint": [host, port],
                })
            }
            "guest" => json!({
                "group": self.group,
                "fingerprint": fingerprint,
                "packages": device_guest::cloud_init_packages(&resolved),
                "write_files": device_guest::cloud_init_write_files(cfg, &resolved)?,
                "runcmd": device_guest::cloud_init_runcmd(cfg, &resolved),
                "alignment_script_sha256": sha256_text(&device_guest::alignment_script(cfg, &resolved)?),
                "guest": resolved.get("guest").cloned().unwrap_or(Value::Null),
            }),
            _ => Value::Null,
        })
    }
}

impl Step for DeviceGroupStep {
    fn id(&self) -> &str {
        // The static id strings are owned in `new`; all five are interned.
        match self.group {
            "cpu" => "device-cpu",
            "smbios" => "device-smbios",
            "devices" => "device-devices",
            "network" => "device-network",
            "guest" => "device-guest",
            _ => "device",
        }
    }
    fn title(&self) -> &str {
        &self.title
    }

    fn check(&self, ctx: &mut StepContext) -> Result<CheckResult> {
        let cfg = ctx.cfg.clone();
        if !cfg.device_enabled() {
            return Ok(CheckResult::ready("device disabled"));
        }
        let state = self.state(&cfg).unwrap_or(Value::Null);
        let meta = state.get("meta").cloned().unwrap_or(Value::Null);
        if state.get("status").and_then(Value::as_str) != Some("applied") {
            return Ok(CheckResult::pending(format!(
                "device {} has not been applied",
                self.group
            )));
        }
        if meta.get("group_fingerprint").and_then(Value::as_str)
            != Some(cfg.device_fingerprint(Some(self.group))?.as_str())
        {
            return Ok(CheckResult::pending(format!(
                "device {} configuration changed",
                self.group
            )));
        }
        let artifact = self.artifact(&cfg);
        if !artifact.exists() {
            return Ok(CheckResult::pending(format!(
                "missing {}",
                artifact
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("artifact")
            )));
        }
        if let Some(expected) = meta.get("artifact_sha256").and_then(Value::as_str)
            && sha256_file(&artifact)? != expected
        {
            return Ok(CheckResult::pending(format!(
                "{} changed on disk",
                artifact
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("artifact")
            )));
        }
        if self.group == "smbios" {
            if !cfg.smbios_path().exists() {
                return Ok(CheckResult::pending("missing smbios.bin"));
            }
            if let Some(expected) = meta.get("smbios_sha256").and_then(Value::as_str)
                && sha256_file(&cfg.smbios_path())? != expected
            {
                return Ok(CheckResult::pending("smbios.bin changed on disk"));
            }
        }
        if let Err(error) = self.validate(&cfg) {
            return Ok(CheckResult::pending(format!("validation failed: {error}")));
        }
        let fingerprint = meta
            .get("group_fingerprint")
            .and_then(Value::as_str)
            .unwrap_or("");
        Ok(CheckResult::ready(format!(
            "device {} ready ({})",
            self.group,
            &fingerprint[..fingerprint.len().min(12)]
        )))
    }

    fn apply(&self, ctx: &mut StepContext) -> Result<()> {
        let cfg = ctx.cfg.clone();
        if !cfg.device_enabled() {
            return Ok(());
        }
        ensure_dir(&cfg.device_dir())?;
        self.validate(&cfg)?;
        if self.group == "smbios" {
            let resolved = resolved(&cfg)?;
            let data = device_smbios::build(&resolved, &cfg.name, cfg.cpus, cfg.memory_mb)?;
            device_smbios::validate_table(&data)?;
            atomic_write_bytes(&cfg.smbios_path(), &data, 0o600)?;
            require_dry_run(
                &cfg,
                &[
                    "-smbios".to_string(),
                    format!("file={}", cfg.smbios_path().display()),
                ],
                "SMBIOS",
            )?;
            let payload = json!({
                "group": self.group,
                "fingerprint": cfg.device_fingerprint(Some(self.group))?,
                "path": cfg.smbios_path().display().to_string(),
                "sha256": sha256_file(&cfg.smbios_path())?,
                "summary": device_smbios::summary(&data)?,
                "smbios": resolved.get("smbios").cloned().unwrap_or(Value::Null),
            });
            let artifact = self.artifact(&cfg);
            atomic_write_json(&artifact, &payload, 0o600)?;
            ctx.track_file(&cfg.smbios_path())?;
            ctx.track_file(&artifact)?;
            ctx.set_meta(
                "group_fingerprint",
                json!(cfg.device_fingerprint(Some(self.group))?),
            );
            ctx.set_meta("artifact_sha256", json!(sha256_file(&artifact)?));
            ctx.set_meta("smbios_sha256", json!(sha256_file(&cfg.smbios_path())?));
            ctx.log(&format!(
                "device smbios rendered: {} ({} bytes)",
                cfg.smbios_path().display(),
                data.len()
            ));
            return Ok(());
        }
        let payload = self.render(&cfg)?;
        let artifact = self.artifact(&cfg);
        atomic_write_json(&artifact, &payload, 0o600)?;
        ctx.track_file(&artifact)?;
        ctx.set_meta(
            "group_fingerprint",
            json!(cfg.device_fingerprint(Some(self.group))?),
        );
        ctx.set_meta("artifact_sha256", json!(sha256_file(&artifact)?));
        ctx.log(&format!(
            "device {} rendered: {}",
            self.group,
            artifact.display()
        ));
        Ok(())
    }

    fn rollback(&self, ctx: &mut StepContext, state: &Value, force: bool) -> Result<()> {
        let cfg = ctx.cfg.clone();
        let artifact = self.artifact(&cfg);
        if artifact.exists() {
            ctx.log(&format!("remove {}", artifact.display()));
            let _ = std::fs::remove_file(&artifact);
        }
        if self.group == "smbios" && cfg.smbios_path().exists() {
            ctx.log(&format!("remove {}", cfg.smbios_path().display()));
            let _ = std::fs::remove_file(cfg.smbios_path());
        }
        delete_artifacts(ctx, state, force)
    }
}

pub struct DeviceSealStep;

impl DeviceSealStep {
    fn reset_efi_vars(&self, cfg: &Config, ctx: &StepContext) {
        if let Ok(resolved) = resolve_qemu(cfg) {
            let tmp = cfg.efi_vars_path().with_file_name(".efi-vars.seal.tmp");
            let result = (|| -> Result<()> {
                std::fs::copy(&resolved.firmware_vars, &tmp)?;
                crate::util::chmod(&tmp, 0o600)?;
                std::fs::rename(&tmp, cfg.efi_vars_path())?;
                Ok(())
            })();
            match result {
                Ok(()) => ctx
                    .log("UEFI variable store reset to the firmware template for the runtime boot"),
                Err(error) => ctx.log(&format!("warning: could not reset UEFI vars: {error}")),
            }
        }
    }

    fn write_marker(&self, cfg: &Config, ctx: &mut StepContext) -> Result<()> {
        atomic_write_json(
            &cfg.device_runtime_marker(),
            &json!({
                "profile_fingerprint": cfg.device_fingerprint(None)?,
                "phase": "runtime",
                "created_at": now_millis(),
            }),
            0o600,
        )?;
        ctx.track_file(&cfg.device_runtime_marker())?;
        Ok(())
    }

    fn probe(&self, cfg: &Config, runtime: &Runtime, ctx: &StepContext) -> Result<(Value, Value)> {
        crate::agentd::ensure_helper(runtime, ctx.helper_binary()?)?;
        for _ in 0..10 {
            let (_, out) = runtime.ssh_capture(
                "timedatectl show -p NTPSynchronized",
                std::time::Duration::from_secs(8),
            );
            if out.trim().ends_with("yes") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        ctx.log("collecting the in-guest device probe");
        let (rc, out) = runtime.ssh_capture(
            &format!("sudo -n {} device-probe", crate::agentd::HELPER_BIN),
            std::time::Duration::from_secs(240),
        );
        if rc != 0 {
            let tail: String = out
                .chars()
                .rev()
                .take(4000)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            return message(format!("guest probe failed (rc={rc}):\n{tail}"));
        }
        let raw: Value = serde_json::from_str(&out).map_err(|error| {
            crate::Error::Message(format!(
                "guest probe returned invalid JSON: {error}\n{}",
                &out[out.len().saturating_sub(4000)..]
            ))
        })?;
        let resolved = resolved(cfg)?;
        let report = device_verify::evaluate(&raw, &resolved, cfg);
        atomic_write_json(&cfg.device_raw_path(), &raw, 0o600)?;
        atomic_write_json(&cfg.device_report_path(), &report, 0o600)?;
        Ok((raw, report))
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
}

impl Step for DeviceSealStep {
    fn id(&self) -> &str {
        "device-seal"
    }
    fn title(&self) -> &str {
        "Align the guest identity, switch to runtime devices, verify and seal"
    }

    fn check(&self, ctx: &mut StepContext) -> Result<CheckResult> {
        let cfg = ctx.cfg.clone();
        if !cfg.device_enabled() {
            return Ok(CheckResult::ready("device disabled"));
        }
        let provision = load_state(&cfg.home, &cfg.name, "provision").unwrap_or(Value::Null);
        if provision.get("status").and_then(Value::as_str) != Some("applied") {
            return Ok(CheckResult::pending("provision is not applied yet"));
        }
        let state = self.state(&cfg).unwrap_or(Value::Null);
        let meta = state.get("meta").cloned().unwrap_or(Value::Null);
        if state.get("status").and_then(Value::as_str) != Some("applied") {
            return Ok(CheckResult::pending("device-seal has not been applied"));
        }
        if meta.get("profile_fingerprint").and_then(Value::as_str)
            != Some(cfg.device_fingerprint(None)?.as_str())
        {
            return Ok(CheckResult::pending("device profile changed after sealing"));
        }
        if !cfg.device_runtime_marker().exists() {
            return Ok(CheckResult::pending("runtime marker is missing"));
        }
        if !cfg.device_report_path().exists() {
            return Ok(CheckResult::pending("device report is missing"));
        }
        let report = read_json(&cfg.device_report_path()).unwrap_or(Value::Null);
        if !report
            .get("acceptable")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(CheckResult::pending("device report is not acceptable"));
        }
        Ok(CheckResult::ready(format!(
            "sealed, report={} errors={} warnings={}",
            report.get("status").and_then(Value::as_str).unwrap_or("?"),
            report.get("errors").and_then(Value::as_u64).unwrap_or(0),
            report.get("warnings").and_then(Value::as_u64).unwrap_or(0)
        )))
    }

    fn apply(&self, ctx: &mut StepContext) -> Result<()> {
        let cfg = ctx.cfg.clone();
        if !cfg.device_enabled() {
            return Ok(());
        }
        ensure_dir(&cfg.device_dir())?;
        if !cfg.smbios_path().exists() {
            return message("SMBIOS artifact is missing; apply the device-smbios step first");
        }
        let provision = load_state(&cfg.home, &cfg.name, "provision").unwrap_or(Value::Null);
        if provision.get("status").and_then(Value::as_str) != Some("applied") {
            return message("provision must be applied before sealing the device profile");
        }
        ensure_stopped(&cfg, ctx)?;
        let runtime = Runtime::new(&cfg);
        let started = std::time::Instant::now();
        let result = (|| -> Result<()> {
            ctx.log("booting install-phase hardware for guest alignment");
            runtime.start(
                &ctx.log_file,
                Boot {
                    serial_log: Some(&cfg.instance_logs_dir().join("serial-seal-install.log")),
                    phase: Some("install"),
                    ..Boot::new(std::time::Duration::from_secs(300))
                },
            )?;
            let alignment = (|| -> Result<()> {
                ctx.log("running guest identity alignment");
                let (rc, out) = runtime.ssh_script(
                    &device_guest::alignment_script(&cfg, &resolved(&cfg)?)?,
                    std::time::Duration::from_secs(600),
                );
                if !out.is_empty() {
                    let tail: String = out
                        .chars()
                        .rev()
                        .take(12000)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                    ctx.log(&tail);
                }
                if rc != 0 {
                    return message(format!("guest alignment failed (rc={rc})"));
                }
                Ok(())
            })();
            ctx.log("shutting down after guest alignment");
            runtime.ssh_capture(
                "sudo -n shutdown -P now",
                std::time::Duration::from_secs(20),
            );
            self.wait_stopped(ctx, &runtime);
            alignment?;

            self.reset_efi_vars(&cfg, ctx);
            self.write_marker(&cfg, ctx)?;
            ctx.log("booting runtime hardware (NVMe/e1000e/no virtio)");
            runtime.start(
                &ctx.log_file,
                Boot {
                    serial_log: Some(&cfg.instance_logs_dir().join("serial-seal-runtime.log")),
                    phase: Some("runtime"),
                    ..Boot::new(std::time::Duration::from_secs(300))
                },
            )?;
            ctx.log("creating a firmware boot entry for the runtime disk");
            let (rc, out) = runtime.ssh_script(
                device_guest::efi_entry_script(&cfg, &resolved(&cfg)?),
                std::time::Duration::from_secs(60),
            );
            if !out.is_empty() {
                let tail: String = out
                    .chars()
                    .rev()
                    .take(4000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                ctx.log(&tail);
            }
            let _ = rc;
            let (_raw, report) = self.probe(&cfg, &runtime, ctx)?;
            ctx.log(&format!(
                "device report: status={} errors={} warnings={} checks={}",
                report.get("status").and_then(Value::as_str).unwrap_or("?"),
                report.get("errors").and_then(Value::as_u64).unwrap_or(0),
                report.get("warnings").and_then(Value::as_u64).unwrap_or(0),
                report.get("checks").and_then(Value::as_u64).unwrap_or(0)
            ));
            if let Some(groups) = report.get("groups").and_then(Value::as_object) {
                for (group, info) in groups {
                    ctx.log(&format!(
                        "  {group}: {} errors={} warnings={}",
                        info.get("status").and_then(Value::as_str).unwrap_or("?"),
                        info.get("errors").and_then(Value::as_u64).unwrap_or(0),
                        info.get("warnings").and_then(Value::as_u64).unwrap_or(0)
                    ));
                    if let Some(findings) = info.get("findings").and_then(Value::as_array) {
                        for finding in findings {
                            let severity = finding
                                .get("severity")
                                .and_then(Value::as_str)
                                .unwrap_or("info");
                            if matches!(severity, "error" | "warning" | "unverified") {
                                ctx.log(&format!(
                                    "    {} {}: {} -- {}",
                                    severity.to_uppercase(),
                                    finding.get("id").and_then(Value::as_str).unwrap_or(""),
                                    finding.get("title").and_then(Value::as_str).unwrap_or(""),
                                    finding.get("detail").and_then(Value::as_str).unwrap_or("")
                                ));
                            }
                        }
                    }
                }
            }
            if !report
                .get("acceptable")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return message(format!(
                    "device verification is not acceptable (errors={}, warnings={}, unverified={}); report: {}",
                    report["errors"], report["warnings"], report["unverified"],
                    cfg.device_report_path().display()
                ));
            }
            ctx.log("shutting down the sealed runtime VM");
            runtime.ssh_capture(
                "sudo -n shutdown -P now",
                std::time::Duration::from_secs(20),
            );
            self.wait_stopped(ctx, &runtime);
            let seal = json!({
                "sealed": true,
                "name": cfg.name,
                "profile_fingerprint": cfg.device_fingerprint(None)?,
                "report": cfg.device_report_path().display().to_string(),
                "status": report.get("status").cloned().unwrap_or(Value::Null),
                "errors": report.get("errors").cloned().unwrap_or(json!(0)),
                "warnings": report.get("warnings").cloned().unwrap_or(json!(0)),
                "checks": report.get("checks").cloned().unwrap_or(json!(0)),
                "runtime_phase": "runtime",
                "duration_s": ((started.elapsed().as_secs_f64() * 10.0).round() / 10.0),
                "sealed_at": now_millis(),
            });
            atomic_write_json(&cfg.device_seal_path(), &seal, 0o600)?;
            ctx.track_file(&cfg.device_seal_path())?;
            ctx.track_file(&cfg.device_report_path())?;
            ctx.track_file(&cfg.device_raw_path())?;
            ctx.set_meta("profile_fingerprint", json!(cfg.device_fingerprint(None)?));
            ctx.set_meta(
                "report_status",
                report.get("status").cloned().unwrap_or(Value::Null),
            );
            ctx.set_meta(
                "report_errors",
                report.get("errors").cloned().unwrap_or(json!(0)),
            );
            ctx.set_meta(
                "report_warnings",
                report.get("warnings").cloned().unwrap_or(json!(0)),
            );
            ctx.set_meta(
                "duration_s",
                seal.get("duration_s").cloned().unwrap_or(json!(0)),
            );
            ctx.log(&format!(
                "device seal complete in {}s",
                seal.get("duration_s")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
            ));
            Ok(())
        })();
        if result.is_err() {
            let serial_tail = tail_file(&cfg.instance_logs_dir().join("serial-seal-runtime.log"), 80);
            if !serial_tail.is_empty() {
                ctx.log(&format!("runtime serial log tail:\n{serial_tail}"));
            }
        }
        if runtime.alive() {
            runtime.stop(&ctx.log_file, std::time::Duration::from_secs(60));
        }
        result
    }

    fn rollback(&self, ctx: &mut StepContext, state: &Value, force: bool) -> Result<()> {
        let cfg = ctx.cfg.clone();
        ensure_stopped(&cfg, ctx)?;
        if cfg.identity_path().exists() {
            ctx.log(&format!(
                "remove {} (restoring the install-time identity)",
                cfg.identity_path().display()
            ));
            let _ = std::fs::remove_file(cfg.identity_path());
        }
        let mut credentials = read_json(&cfg.credentials_path()).unwrap_or(Value::Null);
        if credentials.is_object()
            && (credentials.get("username").and_then(Value::as_str) != Some(cfg.username.as_str())
                || credentials.get("hostname").and_then(Value::as_str)
                    != Some(cfg.hostname.as_str()))
        {
            if let Some(map) = credentials.as_object_mut() {
                map.insert("username".to_string(), json!(cfg.username));
                map.insert("hostname".to_string(), json!(cfg.hostname));
            }
            atomic_write_json(&cfg.credentials_path(), &credentials, 0o600)?;
        }
        if cfg.device_report_path().exists() {
            let archive = cfg.device_dir().join(format!(
                "report-failed-{}.json",
                utcnow().replace(':', "").replace("+0000", "Z")
            ));
            match std::fs::copy(cfg.device_report_path(), &archive) {
                Ok(_) => ctx.log(&format!("archived failed report to {}", archive.display())),
                Err(error) => ctx.log(&format!(
                    "warning: could not archive failed report: {error}"
                )),
            }
        }
        for path in [
            cfg.device_runtime_marker(),
            cfg.device_report_path(),
            cfg.device_raw_path(),
            cfg.device_seal_path(),
        ] {
            if path.exists() {
                ctx.log(&format!("remove {}", path.display()));
                let _ = std::fs::remove_file(path);
            }
        }
        self.reset_efi_vars(&cfg, ctx);
        delete_artifacts(ctx, state, force)?;
        ctx.log("device seal rollback complete; the next apply aligns and seals the guest again");
        Ok(())
    }
}
