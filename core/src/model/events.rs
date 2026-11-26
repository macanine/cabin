use serde::{Deserialize, Serialize};

#[cfg(test)]
use ts_rs::TS;

use super::{
    AgentStatus, AppSettings, ChatAsyncTask, ChatControls, ChatItem, ChatPermissionMode, ChatState,
    ChatTurnUsage, EnvironmentState, InstallStep, MachineStatus, MonitorSample, NetworkStatus,
    OperationRecord, PlanReview, ProxyCatalog, Session, SessionStatus, StartReport, SubagentTask,
    SyncStatus, Workspace,
};

// --------------------------------------------------------- snapshot + events

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub workspaces: Vec<Workspace>,
    pub sessions: Vec<Session>,
    pub agents: Vec<AgentStatus>,
    pub environment: EnvironmentState,
    /// The node catalog and the route are the Mac's; it fills these in.
    #[serde(default)]
    pub proxy: ProxyCatalog,
    #[serde(default)]
    pub network: NetworkStatus,
    pub operations: Vec<OperationRecord>,
    pub settings: AppSettings,
    pub data_dir: String,
    pub hostname: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AppEvent {
    Operation {
        record: OperationRecord,
    },
    SessionOutput {
        id: String,
        generation: u32,
        #[cfg_attr(test, ts(type = "number"))]
        offset: u64,
        data: String,
    },
    TerminalStatus {
        id: String,
        generation: u32,
        status: SessionStatus,
    },
    SessionStatus {
        id: String,
        status: SessionStatus,
    },
    ChatItem {
        id: String,
        item: ChatItem,
    },
    ChatPlan {
        id: String,
        plan: PlanReview,
    },
    ChatSubagent {
        id: String,
        task: SubagentTask,
    },
    ChatTask {
        id: String,
        task: ChatAsyncTask,
    },
    ChatState {
        id: String,
        state: ChatState,
    },
    ChatModel {
        id: String,
        model: Option<String>,
    },
    ChatPermission {
        id: String,
        permission: ChatPermissionMode,
    },
    ChatUsage {
        id: String,
        turn: ChatTurnUsage,
    },
    ChatControls {
        id: String,
        controls: ChatControls,
    },
    Monitor {
        sample: MonitorSample,
    },
    Agents {
        agents: Vec<AgentStatus>,
    },
    /// The Mac's node catalog and gateway route changed.
    Network {
        catalog: ProxyCatalog,
        status: NetworkStatus,
    },
    Environment {
        state: EnvironmentState,
    },
    Library {
        workspaces: Vec<Workspace>,
        sessions: Vec<Session>,
    },
    /// Sent by the Mac app: the machine changed.
    Machine {
        status: MachineStatus,
    },
    /// Sent by the Mac app after (re)connecting to the guest.
    Snapshot {
        snapshot: Box<AppSnapshot>,
    },
    /// Sent by the Mac app about once a second while installing the machine.
    Install {
        steps: Vec<InstallStep>,
    },
    /// Sent by the Mac app about twice a second while the machine starts,
    /// and as the guest link comes up afterwards.
    Start {
        report: Box<StartReport>,
    },
    /// A synced project's state changed (Mac app; see `sync`).
    Sync {
        status: SyncStatus,
    },
}
