//! Machine lifecycle: install, start, stop, destroy, Chrome and desktop
//! glue, readiness checks and diagnostics.

use agent_core::util::{LockExt, RwLockExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use agent_core::model::{
    AppEvent, BrowserEnvironmentView, BrowserSettings, GuestConnection, InstallPlan, InstallStep,
    MachineInstallOptions, MachineState, MachineStatus, MirrorResult, StartState,
};
use agent_core::util::now_millis;
use serde_json::{Value, json};
use vm_core::startup::{Attempt, Phase};

use super::{Host, start_gateway, unavailable};
use crate::app_error::AppError;
use crate::global_network::GlobalNetwork;

/// First boot needs package downloads before the normal internet gate opens.
/// Keep that exception tied to the install instead of changing the route.
fn start_install_gateway(
    network: &GlobalNetwork,
    cfg: &vm_core::config::Config,
) -> Result<bool, AppError> {
    if !cfg.isolated() {
        return Ok(false);
    }
    let _transaction = network.transaction.locked();
    if network.is_up() {
        network.switch(&network.route()?)?;
    }
    network.begin_install();
    if let Err(error) = start_gateway(network, cfg, None) {
        network.end_install();
        return Err(error);
    }
    Ok(true)
}

impl Host {
    // ------------------------------------------------------ machine ops

    /// Test the chosen OS's ARM64 mirrors (its defaults plus any extra URIs).
    pub fn test_mirrors(
        &self,
        os: Option<String>,
        extra: Vec<String>,
    ) -> Result<Vec<MirrorResult>, AppError> {
        let os = vm_core::os::get(os.as_deref().unwrap_or(vm_core::os::DEFAULT))
            .map_err(Self::vm_error)?;
        let mut uris: Vec<String> = os.mirrors().iter().map(|s| s.to_string()).collect();
        for uri in extra {
            let uri = uri.trim().trim_end_matches('/').to_string();
            if !uri.is_empty() && !uris.contains(&uri) {
                uris.push(uri);
            }
        }
        Self::convert(vm_core::mirror::test_mirrors(os, &uris))
    }

    /// Check every step without changing anything.
    pub fn verify(&self) -> Result<Vec<InstallStep>, AppError> {
        Self::convert(self.machine.verify().map_err(Self::vm_error)?)
    }

    /// Re-apply missing steps and start the VM, optionally with a new mirror.
    pub fn resume(
        self: &Arc<Self>,
        change_mirror: bool,
        apt_mirror: Option<String>,
    ) -> Result<MachineStatus, AppError> {
        let me = self.clone();
        self.busy("Installing the machine", || {
            let cfg = self.machine.config()?;
            if vm_core::vm::Runtime::new(&cfg).alive() {
                return vm_core::message("Stop the machine before repairing its installation");
            }
            let install_network = start_install_gateway(&self.network, &cfg)
                .map_err(|error| vm_core::Error::Message(error.message))?;
            let result = self.machine.resume_with_progress(
                change_mirror.then_some(apt_mirror),
                move |steps| {
                    if let Ok(steps) = Self::convert(steps) {
                        me.emit(AppEvent::Install { steps });
                    }
                },
            );
            if install_network {
                self.network.end_install();
            }
            if result.is_ok() && self.start_state() == Some(StartState::Failed) {
                *self.startup.locked() = None;
            }
            result
        })?;
        Ok(self.snapshot().machine)
    }

    pub fn plan(&self, options: MachineInstallOptions) -> Result<InstallPlan, AppError> {
        let plan = self
            .machine
            .plan(&Self::vm_options(options))
            .map_err(|error| AppError {
                code: "invalid".into(),
                message: error.to_string(),
                details: None,
            })?;
        serde_json::from_value(serde_json::to_value(plan).unwrap_or_default())
            .map_err(|error| AppError::internal(error.to_string()))
    }

    /// Stop the running install; it rolls back and reports "cancelled".
    pub fn cancel_install(&self) -> Result<(), AppError> {
        self.cancel_requested.store(true, Ordering::SeqCst);
        if !self.machine.abort_install() {
            self.cancel_requested.store(false, Ordering::SeqCst);
            return Err(unavailable("No install is running"));
        }
        self.update(|status| status.busy = Some("Cancelling the install".into()));
        Ok(())
    }

    pub fn install(
        self: &Arc<Self>,
        options: MachineInstallOptions,
    ) -> Result<MachineStatus, AppError> {
        let options = Self::vm_options(options);
        let me = self.clone();
        let mut cancelled = false;
        let result = self.busy("Installing the machine", || {
            if self.machine.installed() {
                return vm_core::util::message(
                    "The machine is already installed or has files to recover",
                );
            }
            self.cancel_requested.store(false, Ordering::SeqCst);
            self.network
                .reopen_store()
                .map_err(|error| vm_core::Error::Message(error.message))?;
            let cfg = self.machine.install_config(&options)?;
            let install_network = start_install_gateway(&self.network, &cfg)
                .map_err(|error| vm_core::Error::Message(error.message))?;
            let result = self.machine.install_with_progress(&options, move |steps| {
                if let Ok(steps) =
                    serde_json::from_value(serde_json::to_value(steps).unwrap_or_default())
                {
                    me.emit(AppEvent::Install { steps });
                }
            });
            if install_network {
                self.network.end_install();
            }
            if result.is_err() && !self.machine.info().running {
                self.network.stop();
            }
            cancelled = result.is_err()
                && self.cancel_requested.swap(false, Ordering::SeqCst)
                && !self.machine.installed();
            result
        });
        if cancelled {
            return Err(AppError {
                code: "cancelled".into(),
                message: "Install cancelled. The partial machine was removed; the downloaded image is kept.".into(),
                details: None,
            });
        }
        result?;
        Ok(self.snapshot().machine)
    }

    pub fn start(self: &Arc<Self>) -> Result<MachineStatus, AppError> {
        let status = self.status.read_lock().clone();
        if status.state == MachineState::Running && status.connection == GuestConnection::Connected
        {
            return Ok(status);
        }
        if !self.machine.installed() {
            return Err(unavailable("Install the machine first"));
        }
        let attempt = self.begin_start()?;
        self.start_machine(&attempt)?;
        Ok(self.status.read_lock().clone())
    }

    pub(super) fn start_machine(&self, attempt: &Arc<Attempt>) -> Result<(), AppError> {
        let result: Result<(), AppError> = (|| {
            if self.stopping.load(Ordering::Acquire) {
                return Err(unavailable("Cabin is quitting"));
            }
            self.machine
                .open_start_log(attempt)
                .map_err(Self::vm_error)?;
            let cfg = self.machine.config().map_err(Self::vm_error)?;
            if cfg.isolated() {
                if !self.network.is_up() {
                    start_gateway(&self.network, &cfg, None)?;
                }
                attempt.complete(Phase::Gateway, "The gateway and SSH relay are listening");
            } else {
                attempt.skip(Phase::Gateway, "This machine does not use the gateway");
            }
            self.publish_start(attempt);
            self.machine.start_for_app(attempt).map_err(Self::vm_error)
        })();
        match &result {
            Ok(()) => {
                self.set_machine_info(self.machine.configured_info(true));
                attempt.begin(Phase::Daemon);
                self.publish_start(attempt);
            }
            Err(error) => {
                self.finish_start(attempt, Some(error.message.clone()));
                self.refresh_machine();
                if self.status.read_lock().state != MachineState::Running {
                    self.network.stop();
                }
            }
        }
        self.update(|status| {
            status.busy = None;
            if let Err(error) = &result {
                status.connection = GuestConnection::Offline;
                status.error = Some(error.message.clone());
            }
        });
        self.wake();
        result
    }

    pub fn stop(&self) -> Result<MachineStatus, AppError> {
        if self.active_start().is_some() {
            return Err(unavailable(
                "Wait for the machine startup attempt to finish",
            ));
        }
        self.busy("Stopping the machine", || {
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.update(|status| status.connection = GuestConnection::Offline);
            self.sync.disconnect();
            self.disconnect();
            self.fail_pending("The machine is stopping");
            let info = self.machine.stop()?;
            self.network.stop();
            Ok(info)
        })?;
        Ok(self.snapshot().machine)
    }

    /// Chrome is host-managed because setup may need to stop and snapshot the VM.
    pub fn chrome_status(&self) -> Result<Value, AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        let running = self.machine.info().running;
        let current = self.status.read_lock().clone();
        let busy = current.busy.is_some();
        let install = vm_core::chrome_feature::install_log_path(&cfg).map(|path| {
            let lines = std::fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>();
            json!({
                "running": current.busy.as_deref() == Some("Installing Chrome"),
                "logPath": path.display().to_string(),
                "lines": lines,
            })
        });
        let configured = cfg.desktop_chrome && cfg.browser_enabled();
        let probe =
            if configured && running && !busy && current.connection == GuestConnection::Connected {
                Some(
                    vm_core::shell::browser_status(self.machine.home(), &cfg.name)
                        .map_err(Self::vm_error)?,
                )
            } else {
                None
            };
        Ok(json!({
            "configured": configured,
            "installed": probe.as_ref().and_then(|p| p.get("chrome_installed")),
            "running": probe.as_ref().and_then(|p| p.get("chrome_running")),
            "version": probe.as_ref().and_then(|p| p.get("chrome_version")),
            "viewerReady": probe.as_ref().and_then(|p| p.get("viewer_ready")),
            "displayProtocols": if cfg.desktop_spice { vec!["vnc", "spice"] } else { vec!["vnc"] },
            "canEnableSpice": running && !busy && current.connection == GuestConnection::Connected
                && vm_core::chrome_feature::can_enable_spice(&cfg),
            "install": install,
        }))
    }

    /// The browser environment panel's data. `check` also audits the guest
    /// (installed profile and live Chrome; bounded, a few seconds at most).
    pub fn browser_environment_view(
        &self,
        check: bool,
    ) -> Result<BrowserEnvironmentView, AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        let running = self.machine.info().running
            && self.status.read_lock().connection == GuestConnection::Connected;
        if !cfg.browser_enabled() {
            return Ok(BrowserEnvironmentView {
                enabled: false,
                machine_running: running,
                settings: None,
                probe: None,
                live_matches: None,
                checked_at: None,
            });
        }
        let settings = self.machine.browser_settings().map_err(Self::vm_error)?;
        let settings = serde_json::to_value(settings)
            .and_then(serde_json::from_value::<BrowserSettings>)
            .map_err(|error| AppError::internal(error.to_string()))?;
        let probe = (check && running).then(|| Self::audit_browser(&cfg));
        let live_matches = probe.as_ref().and_then(crate::network_gate::live_matches);
        Ok(BrowserEnvironmentView {
            enabled: true,
            machine_running: running,
            settings: Some(settings),
            live_matches,
            checked_at: probe.as_ref().map(|_| now_millis()),
            probe,
        })
    }

    /// Save the browser settings and apply them to the running machine (live
    /// for a region change; otherwise the profile is written again and Chrome
    /// restarts). The environment revision changes, so the network gate
    /// rechecks on its own.
    pub fn browser_settings_save(
        &self,
        settings: BrowserSettings,
    ) -> Result<BrowserEnvironmentView, AppError> {
        let settings: vm_core::browser::Settings = serde_json::to_value(settings)
            .and_then(serde_json::from_value)
            .map_err(|error| AppError::internal(error.to_string()))?;
        self.machine
            .set_browser_settings(&settings)
            .map_err(Self::vm_error)?;
        let _ = self.emit_network();
        self.browser_environment_view(true)
    }

    pub fn install_chrome(&self) -> Result<(), AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        let install_network = start_install_gateway(&self.network, &cfg)?;
        let result = self.busy("Installing Chrome", || {
            self.disconnect();
            self.fail_pending("The machine is restarting to install Chrome");
            if let Ok(cfg) = self.machine.config() {
                let _ = vm_core::chrome_viewer::stop(&cfg);
            }
            self.machine.install_chrome()
        });
        if install_network {
            self.network.end_install();
        }
        result
    }

    pub fn chrome_open(&self, url: &str) -> Result<(), AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        let result = vm_core::shell::browser_open(self.machine.home(), &cfg.name, url)
            .map_err(Self::vm_error)?;
        if result.code != 0 {
            return Err(unavailable(result.output));
        }
        Ok(())
    }

    pub fn chrome_stop(&self) -> Result<(), AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        let result =
            vm_core::shell::browser_stop(self.machine.home(), &cfg.name).map_err(Self::vm_error)?;
        if result.code != 0 {
            return Err(unavailable(result.output));
        }
        Ok(())
    }

    pub fn chrome_paste(&self, text: &str) -> Result<(), AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        vm_core::shell::browser_paste(self.machine.home(), &cfg.name, Some(text))
            .map_err(Self::vm_error)
    }

    pub fn chrome_native_display(&self, request: Value) -> Result<Value, AppError> {
        self.machine
            .browser_native_display(request)
            .map_err(Self::vm_error)
    }

    pub fn chrome_enable_spice(&self) -> Result<(), AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        if !cfg.desktop_spice && !vm_core::chrome_feature::can_enable_spice(&cfg) {
            return Err(unavailable(
                "SPICE requires an installed Sway desktop without the device profile.",
            ));
        }
        self.busy("Enabling SPICE", || {
            self.disconnect();
            self.fail_pending("The machine is restarting to enable SPICE");
            self.machine.enable_spice()
        })
    }

    pub fn chrome_display(
        &self,
        protocol: Option<&str>,
        parent_origin: Option<&str>,
    ) -> Result<Value, AppError> {
        let protocol = protocol.unwrap_or("vnc");
        let url = self
            .machine
            .desktop_viewer(protocol, parent_origin)
            .map_err(Self::vm_error)?;
        Ok(json!({"protocol": protocol, "url": url}))
    }

    /// Forward the guest's `localhost:<port>` to the Mac for a browser preview.
    pub fn preview_forward(&self, port: u16) -> Result<Value, AppError> {
        let url = self.machine.preview_url(port).map_err(Self::vm_error)?;
        Ok(json!({"port": port, "url": url}))
    }

    pub fn preview_close(&self, port: u16) -> Result<(), AppError> {
        self.machine.preview_close(port).map_err(Self::vm_error)
    }

    /// Graphical apps installed in the machine's desktop.
    pub fn desktop_apps(&self) -> Result<Value, AppError> {
        let cfg = self.running_desktop()?;
        let apps = vm_core::desktop::apps(&cfg).map_err(Self::vm_error)?;
        serde_json::to_value(apps).map_err(|error| AppError::internal(error.to_string()))
    }

    /// Start an app on its own workspace of the desktop.
    pub fn desktop_launch(&self, id: &str) -> Result<(), AppError> {
        let cfg = self.running_desktop()?;
        vm_core::desktop::launch(&cfg, id).map_err(Self::vm_error)
    }

    fn running_desktop(&self) -> Result<vm_core::Config, AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        if cfg.desktop_env.is_none() {
            return Err(unavailable("Install Chrome first; it sets up the desktop."));
        }
        if !self.machine.info().running {
            return Err(unavailable("Start the machine to use its desktop."));
        }
        Ok(cfg)
    }

    pub fn destroy(&self) -> Result<MachineStatus, AppError> {
        self.busy("Destroying the environment", || {
            let _guard = self.network.transaction.locked();
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.update(|status| status.connection = GuestConnection::Offline);
            self.sync.disconnect();
            self.disconnect();
            self.fail_pending("The environment is being destroyed");
            self.network.stop();
            self.network
                .suspend_store()
                .map_err(|e| vm_core::Error::Message(e.message))?;
            let result = self.sync.reset_storage(|| self.machine.destroy());
            if let Err(error) = result {
                if let Err(reopen) = self.network.reopen_store() {
                    return Err(vm_core::Error::Message(format!(
                        "{error}; restoring network settings failed: {}",
                        reopen.message
                    )));
                }
                if self.machine.info().running
                    && let Ok(cfg) = self.machine.config()
                    && cfg.isolated()
                    && let Err(restart) = start_gateway(&self.network, &cfg, None)
                {
                    return Err(vm_core::Error::Message(format!(
                        "{error}; restoring the network gateway failed: {}",
                        restart.message
                    )));
                }
                return Err(error);
            } else {
                *self.guest_snapshot.write_lock() = None;
                *self.startup.locked() = None;
                self.update(|status| {
                    status.connection = GuestConnection::Offline;
                    status.guest_version = None;
                });
                self.emit(AppEvent::Install { steps: vec![] });
            }
            Ok(self.machine.info())
        })?;
        Ok(self.snapshot().machine)
    }

    /// Host readiness for an install of the given size.
    pub fn checks(&self, disk_gb: u32, memory_mb: u32) -> vm_core::readiness::ReadinessReport {
        let mut items = vm_core::readiness::host_readiness(self.machine.home(), disk_gb, memory_mb);
        if let Some(position) = items.iter().position(|item| item.id == "disk") {
            items.insert(
                position,
                vm_core::readiness::service_check(self.agentd.is_file()),
            );
        }
        vm_core::readiness::ReadinessReport {
            items,
            host: vm_core::readiness::host_facts(self.machine.home()),
        }
    }

    fn install_result_path(&self) -> PathBuf {
        vm_core::StatePaths::new(self.machine.home())
            .logs()
            .join("last-install.json")
    }

    /// Keep the outcome even if setup stopped before the first step was logged.
    pub fn record_install_result(
        &self,
        started_at: i64,
        result: &Result<MachineStatus, AppError>,
        username: Option<&str>,
    ) {
        let failure = result.as_ref().err();
        let state = match failure.map(|error| error.code.as_str()) {
            None => "succeeded",
            Some("cancelled") => "cancelled",
            Some(_) => "failed",
        };
        let report = serde_json::json!({
            "startedAt": started_at,
            "finishedAt": vm_core::util::now_millis(),
            "state": state,
            "error": failure.map(|error| &error.message),
            "username": username,
        });
        if let Err(error) =
            vm_core::util::atomic_write_json(&self.install_result_path(), &report, 0o600)
        {
            cabin_diagnostics::event(
                cabin_diagnostics::Level::Warn,
                "install",
                "outcome_write_failed",
                format_args!("{error}"),
            );
        }
    }

    /// Collect support information even when the guest is offline.
    pub fn diagnostics(
        &self,
        disk_gb: u32,
        memory_mb: u32,
        steps: Vec<InstallStep>,
        install_error: Option<String>,
        install_user: Option<String>,
    ) -> Result<String, AppError> {
        use vm_core::readiness::{diagnostic_history, host_os_version, redact_diagnostics};

        let home = self.machine.home();
        let machine = self.status.read_lock().clone();
        let start = self
            .startup
            .locked()
            .as_ref()
            .map(|(_, report)| report.clone());
        let mut history = diagnostic_history(home);
        let guest_events = self
            .call_guest_with_timeout(
                "diagnostic_events",
                serde_json::json!({}),
                std::time::Duration::from_secs(1),
            )
            .ok()
            .filter(serde_json::Value::is_array);
        let install = vm_core::util::read_json(&self.install_result_path()).unwrap_or_default();
        let mut recorded_events = history["lastInstall"].take();
        if let Some(started_at) = install["startedAt"].as_i64()
            && recorded_events[0]["ts"]
                .as_i64()
                .is_some_and(|ts| ts < started_at)
        {
            recorded_events = serde_json::json!([]);
        }
        let mut readiness = self.checks(disk_gb, memory_mb);
        let host_user = readiness.host.username.take();
        let platform = &readiness.host.platform;
        let vendor = vm_core::vendor::bundled_manifest().map(|vendor| vendor.manifest);
        let mut report = serde_json::json!({
            "versions": {
                "cabin": env!("CARGO_PKG_VERSION"),
                "bundledCabind": self.agentd.is_file().then_some(env!("CARGO_PKG_VERSION")),
                "runningCabind": machine.guest_version,
                "ubuntu": machine.os,
                "qemu": vendor.as_ref().map(|manifest| &manifest.qemu.version)
                    .map(String::as_str).or_else(|| (platform == "macos" && vm_core::qemu_engine::installed(home).is_some()).then_some(vm_core::qemu_engine::RELEASE)),
                "openSsh": vendor.as_ref().and_then(|manifest| manifest.ssh.as_ref()).map(|ssh| &ssh.version),
            },
            "host": {
                "facts": readiness.host,
                "osVersion": host_os_version(platform),
                "architecture": vm_core::platform::native_arch(),
            },
            "readiness": readiness.items,
            "lastInstall": {
                "steps": steps,
                "state": install["state"],
                "startedAt": install["startedAt"],
                "finishedAt": install["finishedAt"],
                "error": install_error.map(serde_json::Value::String).unwrap_or_else(|| install["error"].clone()),
                "recordedEvents": recorded_events,
            },
            "lastStart": start.as_ref().map(|report| serde_json::json!(report)).unwrap_or_else(|| history["lastStart"].take()),
            "logTails": history["logTails"].take(),
            "events": { "host": history["events"].take(), "guest": guest_events },
        });

        let profile = std::env::home_dir();
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
        let users: Vec<_> = [
            host_user.as_deref(),
            machine.username.as_deref(),
            install_user.as_deref(),
            install["username"].as_str(),
        ]
        .into_iter()
        .flatten()
        .collect();
        let roots: Vec<_> = [Some(home), profile.as_deref(), appdata.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        redact_diagnostics(&mut report, &roots, &users);
        serde_json::to_string_pretty(&report).map_err(|error| AppError::internal(error.to_string()))
    }

    /// Carry out a readiness fix.
    pub fn fix(
        &self,
        fix: vm_core::readiness::Fix,
    ) -> Result<vm_core::readiness::FixOutcome, AppError> {
        vm_core::readiness::apply_fix(&fix).map_err(|error| AppError {
            code: "fixFailed".into(),
            message: error.to_string(),
            details: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::model::NetworkRoute;
    use std::sync::mpsc;
    use std::time::Duration;
    use vm_core::machine::Machine;

    #[test]
    fn failed_auto_start_keeps_a_report_and_retry_gets_a_new_log() {
        let home = std::env::temp_dir().join(format!("cabin-host-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let meta = vm_core::paths::StatePaths::new(&home).instance_meta(vm_core::machine::NAME);
        std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
        std::fs::write(&meta, "{}").unwrap();
        let machine = Machine::new(&home, concat!(env!("CARGO_MANIFEST_DIR"), "/../vm"));
        let host = Host::new(machine, PathBuf::from("/missing/agentd")).unwrap();
        let (events, wait) = mpsc::channel();
        host.set_sink(move |event| {
            let _ = events.send(event);
        });
        host.run_connection_loop().unwrap();
        host.run_connection_loop().unwrap();
        let first = loop {
            let event = wait.recv_timeout(Duration::from_secs(3)).unwrap();
            if let AppEvent::Machine { status } = event
                && status.error.is_some()
                && status.busy.is_none()
            {
                break host.snapshot().start.unwrap();
            }
        };
        assert_eq!(first.state, StartState::Failed);
        assert_eq!(
            first.phases[0].state,
            agent_core::model::StartPhaseState::Failed
        );
        assert!(host.start().is_err());
        let retry = host.snapshot().start.unwrap();
        assert_eq!(retry.state, StartState::Failed);
        assert_ne!(first.log_path, retry.log_path);
        assert!(
            retry
                .log_tail
                .iter()
                .any(|line| line.contains("Startup failed"))
        );
        host.shutdown();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_competing_install_leaves_the_active_operation_and_network_untouched() {
        let home = tempfile::tempdir().unwrap();
        let machine = Machine::new(home.path(), concat!(env!("CARGO_MANIFEST_DIR"), "/../vm"));
        let host = Host::new(machine, PathBuf::from("/missing/agentd")).unwrap();
        host.busy("Stopping the machine", || {
            assert!(host.install(MachineInstallOptions::default()).is_err());
            assert_eq!(
                host.snapshot().machine.busy.as_deref(),
                Some("Stopping the machine")
            );
            assert!(!host.network.is_up());
            Ok(())
        })
        .unwrap();
        assert_eq!(host.snapshot().machine.busy, None);
    }

    #[test]
    fn destroy_resets_host_store_and_machine_files_without_stale_state() {
        let home = std::env::temp_dir().join(format!("cabin-host-destroy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let machine = Machine::new(&home, concat!(env!("CARGO_MANIFEST_DIR"), "/../vm"));
        let host = Host::new(machine, PathBuf::from("/missing/agentd")).unwrap();
        let catalog = host
            .network
            .import(
                "old",
                None,
                Some("proxies:\n  - {name: old, type: socks5, server: 127.0.0.1, port: 1080}\n"),
            )
            .unwrap();
        host.network
            .save(&NetworkRoute {
                enabled: true,
                chain: vec![catalog.nodes[0].id.clone()],
                bind_interface: None,
                ..NetworkRoute::default()
            })
            .unwrap();
        std::fs::create_dir_all(home.join("images")).unwrap();
        std::fs::write(home.join("images/cached"), "image").unwrap();
        let status = host.destroy().unwrap();
        assert_eq!(status.state, MachineState::NotInstalled);
        assert!(!home.join("images").exists());
        assert!(!home.join("network").exists());
        assert!(host.network.catalog().unwrap().nodes.is_empty());
        assert!(!host.network.route().unwrap().enabled);
        host.network.reopen_store().unwrap();
        host.network
            .import(
                "new",
                None,
                Some("proxies:\n  - {name: new, type: socks5, server: 127.0.0.1, port: 1081}\n"),
            )
            .unwrap();
        drop(host);
        let reopened = GlobalNetwork::open(&home.join("network")).unwrap();
        assert_eq!(reopened.catalog().unwrap().sources[0].name, "new");
        drop(reopened);
        let _ = std::fs::remove_dir_all(home);
    }
}
