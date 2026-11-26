use std::sync::{Arc, Mutex, RwLock};

use serde::Deserialize;

use crate::agents;
use crate::chat::ChatSupervisor;
use crate::context::Context;
use crate::environment;
use crate::error::{Error, Result};
use crate::model::{
    AgentKind, AgentStatus, AppEvent, AppSettings, AppSnapshot, OperationRecord, OperationState,
    Session, SessionMode, SetupAction, Workspace,
};
use crate::monitor::Monitor;
use crate::ops::OperationManager;
use crate::paths::AppPaths;
use crate::sessions::{SessionEvent, Supervisor};
use crate::store::Store;
use crate::system::HostSystem;
use crate::util::{LockExt, RwLockExt};

use super::{App, Sink, emit_to, sync_model_prices};

impl App {
    pub fn open(paths: AppPaths) -> Result<Self> {
        crate::fsx::ensure_dir(paths.data())?;
        let legacy_runtime = paths.data().join("agent-host");
        if legacy_runtime.exists() {
            let _ = std::fs::remove_dir_all(legacy_runtime);
        }
        let store = Arc::new(Store::open(&paths.database())?);
        store.mark_interrupted()?;
        let ctx = Context {
            system: Arc::new(HostSystem {
                path: paths.child_path(),
            }),
            paths,
            store: store.clone(),
        };
        Ok(Self::with_context(ctx))
    }

    pub fn with_context(ctx: Context) -> Self {
        let sink: Arc<RwLock<Option<Sink>>> = Arc::new(RwLock::new(None));
        let ops = Arc::new(OperationManager::new(ctx.store.clone()));
        {
            let sink = sink.clone();
            ops.set_sink(move |record| {
                emit_to(
                    &sink,
                    AppEvent::Operation {
                        record: record.clone(),
                    },
                )
            });
        }
        let sessions = Arc::new(Supervisor::new(ctx.paths.data().join("terminal-history")));
        let chats = Arc::new(ChatSupervisor::new(ctx.store.clone()));
        {
            let sink = sink.clone();
            chats.set_sink(move |event| emit_to(&sink, event));
        }
        {
            let sink = sink.clone();
            sessions.set_sink(move |event| {
                emit_to(
                    &sink,
                    match event {
                        SessionEvent::Output {
                            id,
                            generation,
                            offset,
                            data,
                        } => AppEvent::SessionOutput {
                            id,
                            generation,
                            offset,
                            data,
                        },
                        SessionEvent::Status {
                            id,
                            generation,
                            status,
                        } => AppEvent::TerminalStatus {
                            id,
                            generation,
                            status,
                        },
                    },
                )
            });
        }
        Self {
            ctx,
            ops,
            sessions,
            terminal_lifecycle: Mutex::new(()),
            chats,
            sink,
            monitor: Mutex::new(None),
        }
    }

    /// Start background services: the monitor and the model price refresh.
    pub fn start_services(&self) {
        let pricing_ctx = self.ctx.clone();
        std::thread::spawn(move || {
            loop {
                if sync_model_prices(&pricing_ctx, false).is_err() {
                    cabin_diagnostics::event(
                        cabin_diagnostics::Level::Warn,
                        "usage",
                        "price_refresh_failed",
                        format_args!("Could not refresh model prices"),
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(30 * 60));
            }
        });
        let store = self.ctx.store.clone();
        let sink = self.sink.clone();
        let monitor = Monitor::start(
            move || {
                store
                    .setting::<AppSettings>("app")
                    .ok()
                    .flatten()
                    .unwrap_or_default()
                    .latency_target
            },
            move |sample| emit_to(&sink, AppEvent::Monitor { sample }),
        );
        *self.monitor.locked() = Some(monitor);
    }

    /// The busiest processes, for the performance details (on request only).
    pub fn top_processes(&self, limit: usize) -> Result<crate::model::ProcessList> {
        let monitor = self.monitor.locked();
        let monitor = monitor
            .as_ref()
            .ok_or_else(|| Error::Internal("The monitor is not running".into()))?;
        Ok(monitor.top_processes(limit.clamp(1, 50)))
    }

    pub fn shutdown(&self) {
        self.sessions.stop_all();
        self.chats.stop_all();
        self.monitor.locked().take();
    }

    pub fn set_event_sink(&self, sink: impl Fn(AppEvent) + Send + Sync + 'static) {
        *self.sink.write_lock() = Some(Arc::new(sink));
    }

    pub(super) fn emit(&self, event: AppEvent) {
        emit_to(&self.sink, event);
    }

    pub fn context(&self) -> &Context {
        &self.ctx
    }

    // -------------------------------------------------------------- snapshot

    pub fn snapshot(&self) -> Result<AppSnapshot> {
        let _ = agents::codex_accounts(&self.ctx);
        self.snapshot_with_agents(agents::all_status(&self.ctx))
    }

    pub fn startup_snapshot(&self) -> Result<AppSnapshot> {
        self.snapshot_with_agents(agents::inventory(&self.ctx))
    }

    pub fn ensure_ipv4_network(&self) -> Result<()> {
        crate::guest_ipv4::ensure(&self.ctx, &self.ops)
    }

    fn snapshot_with_agents(&self, agents: Vec<AgentStatus>) -> Result<AppSnapshot> {
        let (workspaces, sessions) = self.library()?;
        Ok(AppSnapshot {
            workspaces,
            sessions,
            agents,
            environment: environment::state(&self.ctx),
            // The Mac app owns and fills in the catalog and the route.
            proxy: Default::default(),
            network: Default::default(),
            operations: self.ops.recent(50)?,
            settings: self.ctx.settings(),
            data_dir: self.ctx.paths.data().display().to_string(),
            hostname: std::fs::read_to_string("/etc/hostname")
                .map(|name| name.trim().to_string())
                .ok()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "This machine".into()),
        })
    }

    fn library(&self) -> Result<(Vec<Workspace>, Vec<Session>)> {
        let workspaces = self.ctx.store.workspaces()?;
        let mut sessions = self.ctx.store.sessions()?;
        for session in &mut sessions {
            session.status = if session.mode == SessionMode::Ui {
                self.chats.status(&session.id)
            } else {
                self.sessions.status(&session.id)
            };
        }
        Ok((workspaces, sessions))
    }

    pub(super) fn emit_library(&self) -> Result<()> {
        let (workspaces, sessions) = self.library()?;
        self.emit(AppEvent::Library {
            workspaces,
            sessions,
        });
        Ok(())
    }

    // ------------------------------------------------------------ operations

    pub fn operations(&self) -> Result<Vec<OperationRecord>> {
        self.ops.recent(50)
    }

    pub fn cancel_operation(&self, id: &str) -> Result<()> {
        self.ops.cancel(id)
    }

    /// Re-run a finished operation from its stored request.
    pub fn retry_operation(&self, id: &str) -> Result<OperationRecord> {
        let previous = self
            .ops
            .recent(200)?
            .into_iter()
            .find(|record| record.id == id)
            .ok_or_else(|| Error::NotFound("operation".into()))?;
        if !previous.state.is_finished() || previous.state == OperationState::Succeeded {
            return Err(Error::Conflict(
                "Only failed operations can be retried".into(),
            ));
        }
        let (kind, request) = self.ctx.store.operation_request(id)?;
        let request =
            request.ok_or_else(|| Error::Conflict("This operation cannot be retried".into()))?;
        #[derive(Deserialize)]
        struct AgentRequest {
            agent: AgentKind,
            #[serde(default)]
            model: Option<String>,
        }
        match kind.as_str() {
            "environment.apply" => self.apply_environment(serde_json::from_value(request)?),
            "agent.setModel" => {
                let request: AgentRequest = serde_json::from_value(request)?;
                self.set_agent_model(request.agent, request.model)
            }
            "setup.installAgent" => {
                let request: AgentRequest = serde_json::from_value(request)?;
                self.run_setup_action(&SetupAction::InstallAgent {
                    agent: request.agent,
                })
            }
            "acp.install" => {
                #[derive(Deserialize)]
                struct InstallRequest {
                    agent: AgentKind,
                    version: String,
                    #[serde(default)]
                    select: bool,
                }
                let request: InstallRequest = serde_json::from_value(request)?;
                self.acp_runtime_install(request.agent, &request.version, request.select)
            }
            "setup.installBase" => self.run_setup_action(&SetupAction::InstallBasePackages),
            "python.install" => self.run_setup_action(&SetupAction::InstallPython),
            "node.install" => self.node_install(serde_json::from_value(request)?),
            "node.use" | "node.remove" => {
                #[derive(Deserialize)]
                struct VersionRequest {
                    version: String,
                }
                let request: VersionRequest = serde_json::from_value(request)?;
                if kind == "node.use" {
                    self.node_use(&request.version)
                } else {
                    self.node_remove(&request.version)
                }
            }
            other => Err(Error::Conflict(format!(
                "Operations of kind {other} cannot be retried"
            ))),
        }
    }

    // -------------------------------------------------------------- settings

    pub fn update_settings(&self, settings: AppSettings) -> Result<AppSettings> {
        for (label, target) in [
            ("latency target", &settings.latency_target),
            ("connectivity check target", &settings.check_target),
        ] {
            let valid = target
                .rsplit_once(':')
                .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
            if !valid {
                return Err(Error::invalid(format!(
                    "The {label} must look like host:port"
                )));
            }
        }
        self.ctx.store.set_setting("app", &settings)?;
        Ok(settings)
    }
}
