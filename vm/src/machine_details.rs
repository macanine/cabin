//! A read-only view of the VM for the Mac settings page.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::util::{read_json, timestamp_millis};
use crate::{Config, Result};

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct MachineDetails {
    pub os_name: String,
    pub accelerator: String,
    pub qemu_release: Option<String>,
    pub image_url: Option<String>,
    pub image_checksum: Option<String>,
    pub apt_mirror: Option<String>,
    pub disk_path: String,
    #[cfg_attr(feature = "bindings", ts(type = "number | null"))]
    pub disk_allocated_bytes: Option<u64>,
    pub pending_resources: Option<crate::resources::ResourceRequest>,
    pub disk_growth_notice: Option<String>,
    pub instance_path: String,
    pub logs_path: String,
    pub network_mode: String,
    pub ssh_endpoint: Option<String>,
    pub mac: String,
    pub timezone: String,
    pub locale: String,
    pub desktop: Option<String>,
    pub display_mode: String,
    pub resolution: String,
    pub scale: f64,
    pub gpu: String,
    pub audio: String,
    pub spice: bool,
    pub pid: Option<u32>,
    #[cfg_attr(feature = "bindings", ts(type = "number | null"))]
    pub started_at: Option<i64>,
    pub device: Option<DeviceDetails>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum DeviceReportState {
    Missing,
    Stale,
    Unverified,
    Failed,
    Warnings,
    Passed,
}

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DeviceFinding {
    pub id: String,
    pub group: String,
    pub severity: String,
    pub title: String,
    pub residual: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DeviceDetails {
    pub preset: Option<String>,
    pub cpu: Option<String>,
    pub storage: Option<String>,
    pub nic: Option<String>,
    pub runtime: bool,
    pub sealed: bool,
    pub state: DeviceReportState,
    #[cfg_attr(feature = "bindings", ts(type = "number | null"))]
    pub measured_at: Option<i64>,
    pub report_path: Option<String>,
    pub findings: Vec<DeviceFinding>,
}

impl MachineDetails {
    pub(crate) fn read(cfg: &Config) -> Result<Self> {
        let mode = cfg.effective_network_mode();
        let device = cfg
            .device_enabled()
            .then(|| DeviceDetails::read(cfg))
            .transpose()?;
        let runtime = crate::vm::Runtime::new(cfg);
        let pid = runtime.pid().filter(|pid| crate::vm::pid_alive(Some(*pid)));
        let started_at = pid.and_then(|pid| {
            let record = read_json(&cfg.runtime_path())?;
            (record["pid"].as_u64() == Some(u64::from(pid)))
                .then(|| timestamp_millis(&record["started_at"]))
                .flatten()
        });
        let ssh_endpoint = if device.is_some() {
            crate::device::render::ssh_endpoint(cfg, None).ok()
        } else {
            Some(("127.0.0.1".into(), cfg.ssh_connect_port()))
        };
        let disk_path = cfg.disk_path();
        let disk_allocated_bytes = std::fs::metadata(&disk_path)
            .ok()
            .map(|metadata| crate::storage::allocated_bytes(&metadata));
        let image_checksum = if !cfg.image_sha256.is_empty() {
            Some(format!("SHA-256 {}", cfg.image_sha256))
        } else if !cfg.image_sha512.is_empty() {
            Some(format!("SHA-512 {}", cfg.image_sha512))
        } else {
            None
        };
        Ok(Self {
            os_name: cfg.guest_os().name.to_string(),
            accelerator: if device.is_some() {
                "hvf"
            } else {
                crate::platform::accel().accel
            }
            .into(),
            qemu_release: cfg!(target_os = "macos")
                .then(|| crate::qemu_engine::RELEASE.to_string()),
            image_url: cfg
                .image_pinned()
                .then(|| public_url(&cfg.image_url))
                .flatten(),
            image_checksum,
            apt_mirror: cfg.apt_mirror.as_deref().and_then(public_url),
            disk_allocated_bytes,
            pending_resources: crate::resources::pending(cfg)?,
            disk_growth_notice: crate::resources::notice(cfg),
            disk_path: disk_path.display().to_string(),
            logs_path: cfg.instance_logs_dir().display().to_string(),
            instance_path: cfg.instance_dir().display().to_string(),
            mac: if cfg.needs_data_nic() {
                cfg.mac_data()
            } else {
                cfg.mac_mgmt()
            },
            network_mode: mode,
            ssh_endpoint: ssh_endpoint.map(|(host, port)| format!("{host}:{port}")),
            timezone: cfg.current_timezone(),
            locale: cfg.current_locale(),
            desktop: cfg.desktop_env.clone(),
            display_mode: cfg.display_mode.clone(),
            resolution: cfg.desktop_resolution.clone(),
            scale: cfg.desktop_scale,
            gpu: cfg.gpu_mode.clone(),
            audio: cfg.audio_backend.clone(),
            spice: cfg.desktop_spice,
            pid,
            started_at,
            device,
        })
    }
}

impl DeviceDetails {
    fn read(cfg: &Config) -> Result<Self> {
        let resolved = cfg.device_resolved()?;
        let profile = cfg.device_fingerprint(None)?;
        let path = cfg.device_report_path();
        let runtime = cfg.device_runtime_marker().is_file();
        let seal = read_json(&cfg.device_seal_path());
        let report = read_json(&path);
        let sealed = runtime
            && seal.is_some_and(|seal| {
                seal["sealed"].as_bool() == Some(true)
                    && seal["profile_fingerprint"].as_str() == Some(&profile)
            });
        let state = report_state(report.as_ref(), path.is_file(), &profile, sealed);
        let read = |pointer: &str| {
            resolved
                .pointer(pointer)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let findings = report
            .iter()
            .filter_map(|report| report["groups"].as_object())
            .flat_map(|groups| groups.values())
            .filter_map(|group| group["findings"].as_array())
            .flatten()
            .filter_map(|finding| DeviceFinding::deserialize(finding).ok())
            .collect();
        Ok(Self {
            preset: read("/preset"),
            cpu: read("/cpu/model_name"),
            storage: read("/devices/storage/controller"),
            nic: read("/network/nic_model"),
            runtime,
            sealed,
            state,
            measured_at: report
                .as_ref()
                .and_then(|report| timestamp_millis(&report["generated_at"])),
            report_path: path.is_file().then(|| path.display().to_string()),
            findings,
        })
    }
}

fn report_state(
    report: Option<&Value>,
    exists: bool,
    profile: &str,
    sealed: bool,
) -> DeviceReportState {
    use DeviceReportState::*;
    let Some(report) = report else {
        return if exists { Unverified } else { Missing };
    };
    if report["profile_fingerprint"].as_str() != Some(profile) {
        Stale
    } else if report["status"].as_str() == Some("fail") {
        Failed
    } else if report["unverified"].as_u64() != Some(0) {
        Unverified
    } else if report["acceptable"].as_bool() == Some(false) {
        Failed
    } else if sealed && report["acceptable"].as_bool() == Some(true) {
        match report["status"].as_str() {
            Some("pass") => Passed,
            Some("warn") => Warnings,
            _ => Unverified,
        }
    } else {
        Unverified
    }
}

fn public_url(value: &str) -> Option<String> {
    let mut url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    Some(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn device_summary_never_promotes_stale_or_incomplete_evidence() {
        let mut report =
            json!({"profile_fingerprint": "current", "status": "pass", "acceptable": true});
        assert_eq!(
            report_state(None, false, "current", false),
            DeviceReportState::Missing
        );
        assert_eq!(
            report_state(Some(&report), true, "current", true),
            DeviceReportState::Unverified
        );
        report["unverified"] = json!(0);
        assert_eq!(
            report_state(Some(&report), true, "current", false),
            DeviceReportState::Unverified
        );
        assert_eq!(
            report_state(Some(&report), true, "new", true),
            DeviceReportState::Stale
        );
        assert_eq!(
            report_state(Some(&report), true, "current", true),
            DeviceReportState::Passed
        );
        report["status"] = json!("warn");
        assert_eq!(
            report_state(Some(&report), true, "current", true),
            DeviceReportState::Warnings
        );
        report["acceptable"] = json!(false);
        assert_eq!(
            report_state(Some(&report), true, "current", true),
            DeviceReportState::Failed
        );
    }

    #[test]
    fn machine_details_never_include_url_credentials() {
        let url = "https://user:secret@mirror.example/ubuntu?token=secret#secret";
        assert_eq!(
            public_url(url).as_deref(),
            Some("https://mirror.example/ubuntu")
        );
    }
}
