//! Cabin core: all product logic for the desktop app.

pub mod agent_runtime;
pub mod agents;
pub mod browser;
pub mod chat;
pub mod chat_view;
pub mod context;
pub mod device_probe;
pub mod environment;
pub mod error;
pub mod fetch;
pub mod fsx;
pub mod guest_desktop;
pub mod guest_dns;
pub mod guest_ipv4;
pub mod guest_system;
pub mod ipv4;
pub mod model;
pub mod monitor;
pub mod native_display;
pub mod network_probe;
pub mod node;
pub mod ops;
pub mod paths;
pub mod proxy;
pub mod python;
pub mod rpc;
pub mod sessions;
pub mod setup;
pub mod store;
pub mod sync;
pub mod system;
pub mod util;
pub mod workbench;
pub mod workspace_diff;
pub mod worktree;

pub use error::{Error, Result};
pub mod app;

#[cfg(test)]
mod bindings {
    use crate::model::*;
    use ts_rs::{Config, TS};

    /// Regenerate with `CABIN_UPDATE_IPC_BINDINGS=1 cargo test ipc_bindings`.
    #[test]
    fn ipc_bindings_are_current() -> Result<(), Box<dyn std::error::Error>> {
        let config = Config::default();
        let decls = [
            AgentKind::decl(&config),
            ModelOverride::decl(&config),
            Workspace::decl(&config),
            SessionStatus::decl(&config),
            TerminalSnapshot::decl(&config),
            SessionMode::decl(&config),
            ChatQuestion::decl(&config),
            ChatAttachment::decl(&config),
            ChatAttachmentInput::decl(&config),
            ChatAttachmentData::decl(&config),
            ChatItemKind::decl(&config),
            ChatItemStatus::decl(&config),
            ChatRunState::decl(&config),
            ChatPermissionMode::decl(&config),
            AgentModel::decl(&config),
            ClaudeAuthMethod::decl(&config),
            ClaudeAccount::decl(&config),
            ClaudeLogin::decl(&config),
            ClaudeUsageWindow::decl(&config),
            ClaudeUsage::decl(&config),
            CodexAccount::decl(&config),
            CodexQuotaWindow::decl(&config),
            CodexCredits::decl(&config),
            CodexResetCard::decl(&config),
            CodexQuota::decl(&config),
            CodexAccounts::decl(&config),
            CodexResetResult::decl(&config),
            ToolAction::decl(&config),
            ApprovalDecisionView::decl(&config),
            ApprovalView::decl(&config),
            ChangeOrigin::decl(&config),
            ChangeKind::decl(&config),
            FileChangeView::decl(&config),
            FsEntryKind::decl(&config),
            FsEntry::decl(&config),
            FsListing::decl(&config),
            FsContentKind::decl(&config),
            FsFile::decl(&config),
            GitFileStatus::decl(&config),
            GitStatus::decl(&config),
            GitFileDiff::decl(&config),
            GitCommitResult::decl(&config),
            ListeningPort::decl(&config),
            AgentChatCapabilities::decl(&config),
            ChatPromptCapabilities::decl(&config),
            ChatRuntimeCapabilities::decl(&config),
            ChatUsage::decl(&config),
            ChatTurnUsage::decl(&config),
            cabin_usage::Price::decl(&config),
            cabin_usage::Filter::decl(&config),
            cabin_usage::Counts::decl(&config),
            cabin_usage::Log::decl(&config),
            cabin_usage::Daily::decl(&config),
            cabin_usage::Model::decl(&config),
            cabin_usage::Provider::decl(&config),
            cabin_usage::AgentUsage::decl(&config),
            cabin_usage::pricing::SourceStatus::decl(&config),
            cabin_usage::Dashboard::decl(&config),
            cabin_usage::SyncResult::decl(&config),
            ChatItem::decl(&config),
            PlanReviewKind::decl(&config),
            PlanReviewStatus::decl(&config),
            PlanExecutionStatus::decl(&config),
            PlanDecision::decl(&config),
            PlanReview::decl(&config),
            PlanRespondOutcome::decl(&config),
            PlanRespondResult::decl(&config),
            SubagentState::decl(&config),
            SubagentTask::decl(&config),
            ChatState::decl(&config),
            AcpRuntime::decl(&config),
            ChatConfigChoice::decl(&config),
            ChatConfigOption::decl(&config),
            ChatCommand::decl(&config),
            ChatPlanEntry::decl(&config),
            ChatContextUse::decl(&config),
            ChatGoal::decl(&config),
            ChatControls::decl(&config),
            ChatAsyncTask::decl(&config),
            ChatSnapshot::decl(&config),
            crate::worktree::WorktreeRefs::decl(&config),
            SessionWorktree::decl(&config),
            Session::decl(&config),
            OperationState::decl(&config),
            StepState::decl(&config),
            StepRecord::decl(&config),
            OperationRecord::decl(&config),
            EnvironmentSpec::decl(&config),
            EnvironmentState::decl(&config),
            FieldState::decl(&config),
            EnvironmentReview::decl(&config),
            ProxySource::decl(&config),
            ProxyNode::decl(&config),
            ProxyGroup::decl(&config),
            ProxyCatalog::decl(&config),
            ProxyTestKind::decl(&config),
            ProxyTestResult::decl(&config),
            NetworkRoute::decl(&config),
            NetworkStatus::decl(&config),
            NetworkGateMode::decl(&config),
            NetworkConnectivity::decl(&config),
            NetworkTraffic::decl(&config),
            NetworkHistoryEntry::decl(&config),
            IpType::decl(&config),
            IpClassification::decl(&config),
            NetworkCheckStatus::decl(&config),
            NetworkCheckCategory::decl(&config),
            NetworkEvidence::decl(&config),
            NetworkCheckItem::decl(&config),
            NetworkCheckReport::decl(&config),
            GuestNetworkProbe::decl(&config),
            BrowserEnvironmentState::decl(&config),
            BrowserEnvironmentProbe::decl(&config),
            BrowserLive::decl(&config),
            BrowserSettings::decl(&config),
            BrowserEnvironmentView::decl(&config),
            GeoInfo::decl(&config),
            HopCheck::decl(&config),
            RouteCheck::decl(&config),
            NodePlace::decl(&config),
            ProbeResult::decl(&config),
            AgentStatus::decl(&config),
            AgentConfigText::decl(&config),
            McpTransport::decl(&config),
            McpServer::decl(&config),
            crate::agents::mcp_library::McpLibrary::decl(&config),
            crate::agents::mcp_library::McpAgent::decl(&config),
            crate::agents::mcp_library::McpRow::decl(&config),
            crate::agents::mcp_library::McpBinding::decl(&config),
            crate::agents::mcp_library::McpBindingState::decl(&config),
            crate::agents::mcp_library::McpChange::decl(&config),
            crate::agents::skill_library::SkillRepository::decl(&config),
            crate::agents::skill_library::SkillOrigin::decl(&config),
            crate::agents::skill_library::DiscoveredSkill::decl(&config),
            crate::agents::skill_library::RegistrySkill::decl(&config),
            crate::agents::skill_library::SkillRegistry::decl(&config),
            crate::agents::skill_library::SkillMode::decl(&config),
            crate::agents::skill_library::ManagedSkill::decl(&config),
            crate::agents::skill_library::SkillLibrary::decl(&config),
            crate::agents::skill_library::InstalledSkill::decl(&config),
            crate::agents::skill_library::SkillBinding::decl(&config),
            crate::agents::skill_library::LocalSkill::decl(&config),
            crate::agents::skill_library::SkillBackup::decl(&config),
            crate::agents::skill_library::SkillInstall::decl(&config),
            crate::agents::skill_library::SkillChange::decl(&config),
            SkillEntry::decl(&config),
            SkillFile::decl(&config),
            AgentExtensions::decl(&config),
            AgentCommand::decl(&config),
            McpStatus::decl(&config),
            AgentCatalog::decl(&config),
            BackupEntry::decl(&config),
            HealthReport::decl(&config),
            CheckStatus::decl(&config),
            SetupAction::decl(&config),
            SetupCheck::decl(&config),
            NodeSource::decl(&config),
            NodeRuntime::decl(&config),
            NodeInstalled::decl(&config),
            NodeState::decl(&config),
            NodeRelease::decl(&config),
            GuestClock::decl(&config),
            GuestMachineObservation::decl(&config),
            MonitorSample::decl(&config),
            LoadAverage::decl(&config),
            ProcessInfo::decl(&config),
            ProcessList::decl(&config),
            AppSettings::decl(&config),
            AppSnapshot::decl(&config),
            AppEvent::decl(&config),
            MachineState::decl(&config),
            GuestConnection::decl(&config),
            MachineStatus::decl(&config),
            MachineInstallOptions::decl(&config),
            HostCheck::decl(&config),
            StartFact::decl(&config),
            StartPhaseState::decl(&config),
            StartPhase::decl(&config),
            StartState::decl(&config),
            StartReport::decl(&config),
            HostSnapshot::decl(&config),
            DownloadProgress::decl(&config),
            InstallStep::decl(&config),
            InstallPlan::decl(&config),
            MirrorResult::decl(&config),
            OsChoice::decl(&config),
            crate::sync::SyncMode::decl(&config),
            crate::sync::GitHandling::decl(&config),
            crate::sync::SyncRules::decl(&config),
            SyncProject::decl(&config),
            SyncProjectInput::decl(&config),
            SyncState::decl(&config),
            SyncStatus::decl(&config),
            SyncView::decl(&config),
            SyncPreviewItem::decl(&config),
            SyncPreview::decl(&config),
            crate::app::WorkspacePatch::decl(&config),
            crate::app::SessionPatch::decl(&config),
        ];
        let mut generated = String::from(
            "// Generated from core/src/model.rs; regenerate with\n// CABIN_UPDATE_IPC_BINDINGS=1 cargo test ipc_bindings (in core/).\n\n",
        );
        for decl in decls {
            generated.push_str("export ");
            for (index, line) in decl.lines().enumerate() {
                if index > 0 {
                    generated.push('\n');
                }
                generated.push_str(line.trim_end());
            }
            generated.push('\n');
        }
        let path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/services/ipc/generated/core.ts"
        ));
        if std::env::var("CABIN_UPDATE_IPC_BINDINGS").as_deref() == Ok("1") {
            std::fs::create_dir_all(path.parent().ok_or("no parent")?)?;
            std::fs::write(path, &generated)?;
        }
        assert_eq!(
            std::fs::read_to_string(path).unwrap_or_default(),
            generated,
            "TypeScript bindings are stale; run CABIN_UPDATE_IPC_BINDINGS=1 cargo test ipc_bindings"
        );
        Ok(())
    }
}
