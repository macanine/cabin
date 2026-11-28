import type {
  AcpRuntime,
  AgentCatalog,
  AgentChatCapabilities,
  AgentExtensions,
  AgentKind,
  AgentStatus,
  BackupEntry,
  McpServer,
  OperationRecord,
  ClaudeUsage,
  CodexAccounts,
  CodexResetResult,
  HealthReport,
  Session,
  SkillFile,
} from "./generated/core";
import { invokeGuest } from "./common";

const claudeUsageRequests = new Map<string, Promise<ClaudeUsage>>();
let lastClaudeUsage: { accountId: string; usage: ClaudeUsage; receivedAt: number } | null = null;

export const agentsIpc = {
  openTerminal: (kind: AgentKind, login: boolean, cols = 100, rows = 30) =>
    invokeGuest<Session>("agent_terminal_open", { kind, login, cols, rows }),
  /** MCP servers and skills on disk (project skills too with a workspace). */
  extensions: (kind: AgentKind, workspaceId: string | null = null) =>
    invokeGuest<AgentExtensions>("agent_extensions", { kind, workspaceId }),
  /** What the agent itself offers in a workspace: commands, skills, MCP state. */
  catalog: (workspaceId: string, kind: AgentKind, sessionId: string | null = null) => invokeGuest<AgentCatalog>("agent_catalog", { workspaceId, kind, sessionId }),
  saveMcp: (kind: AgentKind, server: McpServer, previous: string | null) =>
    invokeGuest<OperationRecord>("agent_mcp_save", { kind, server, previous }),
  removeMcp: (kind: AgentKind, name: string) => invokeGuest<OperationRecord>("agent_mcp_remove", { kind, name }),
  setMcpEnabled: (kind: AgentKind, name: string, enabled: boolean) =>
    invokeGuest<OperationRecord>("agent_mcp_set_enabled", { kind, name, enabled }),
  copyMcp: (kind: AgentKind, name: string, targets: AgentKind[]) =>
    invokeGuest<OperationRecord>("agent_mcp_copy", { kind, name, targets }),
  readSkill: (kind: AgentKind, path: string, workspaceId: string | null = null) =>
    invokeGuest<SkillFile>("agent_skill_read", { kind, path, workspaceId }),
  deleteSkill: (kind: AgentKind, path: string, workspaceId: string | null = null) =>
    invokeGuest<OperationRecord>("agent_skill_delete", { kind, path, workspaceId }),
  copySkill: (kind: AgentKind, path: string, targets: AgentKind[], workspaceId: string | null = null) =>
    invokeGuest<OperationRecord>("agent_skill_copy", { kind, path, targets, workspaceId }),
  refresh: () => invokeGuest<AgentStatus[]>("agents_refresh"),
  backups: (kind: AgentKind) => invokeGuest<BackupEntry[]>("agent_backups", { kind }),
  restoreBackup: (kind: AgentKind, path: string) =>
    invokeGuest<OperationRecord>("agent_restore_backup", { kind, path }),
  /** The ACP runtime (adapter) each agent's chats run on, and its versions on disk. */
  runtimes: () => invokeGuest<AcpRuntime[]>("acp_runtimes"),
  /** Versions published on npm, newest first. */
  runtimeVersions: (agent: AgentKind) => invokeGuest<string[]>("acp_runtime_versions", { agent }),
  installRuntime: (agent: AgentKind, version: string, select: boolean) =>
    invokeGuest<OperationRecord>("acp_runtime_install", { agent, version, select }),
  selectRuntime: (agent: AgentKind, version: string) => invokeGuest<void>("acp_runtime_select", { agent, version }),
  removeRuntime: (agent: AgentKind, version: string) => invokeGuest<void>("acp_runtime_remove", { agent, version }),
  /** What a chat with each agent can do; the UI hides unsupported actions from this. */
  chatCapabilities: () => invokeGuest<AgentChatCapabilities[]>("agent_chat_capabilities"),
  health: (kind: AgentKind) => invokeGuest<HealthReport>("agent_health", { kind }),
  claudeUsage: (accountId: string) => {
    const pending = claudeUsageRequests.get(accountId);
    if (pending) return pending;
    const now = Date.now();
    if (lastClaudeUsage?.accountId === accountId && performance.now() - lastClaudeUsage.receivedAt < 60_000 &&
        lastClaudeUsage.usage.windows.every(({ resetsAt }) => resetsAt == null || resetsAt > now)) {
      return Promise.resolve(lastClaudeUsage.usage);
    }
    const request = invokeGuest<ClaudeUsage>("claude_usage", { accountId }).then((usage) => {
      lastClaudeUsage = { accountId, usage, receivedAt: performance.now() };
      return usage;
    }).finally(() => { claudeUsageRequests.delete(accountId); });
    claudeUsageRequests.set(accountId, request);
    return request;
  },
  codexAccounts: () => invokeGuest<CodexAccounts>("codex_accounts"),
  importCodexAccount: (contents: string | null, activate = true) =>
    invokeGuest<CodexAccounts>("codex_import_account", { contents, activate }),
  exportCodexAccount: (accountId: string, format: "native" | "cliproxyapi" | "sub2api") =>
    invokeGuest<string>("codex_export_account", { accountId, format }),
  switchCodexAccount: (accountId: string) =>
    invokeGuest<CodexAccounts>("codex_switch_account", { accountId }),
  removeCodexAccount: (accountId: string) =>
    invokeGuest<CodexAccounts>("codex_remove_account", { accountId }),
  refreshCodexQuota: () => invokeGuest<CodexAccounts>("codex_refresh_quota"),
  useCodexResetCard: (creditId: string) =>
    invokeGuest<CodexResetResult>("codex_use_reset_card", {
      creditId,
      confirmed: true,
    }),
};
