// Generated from core/src/model.rs; regenerate with
// CABIN_UPDATE_IPC_BINDINGS=1 cargo test ipc_bindings (in core/).

export type AgentKind = "claude" | "codex" | "opencode" | "pi" | "antigravity";
export type ModelOverride = { agent: AgentKind, model: string, };
export type Workspace = { id: string, name: string, path: string, modelOverrides: Array<ModelOverride>, archived: boolean, createdAt: number, updatedAt: number, };
export type SessionStatus = { "state": "stopped" } | { "state": "starting" } | { "state": "running", pid: number, } | { "state": "exited", code: number | null, } | { "state": "failed", message: string, };
export type TerminalSnapshot = { generation: number, status: SessionStatus, startOffset: number, endOffset: number, data: string, };
export type SessionMode = "terminal" | "ui";
export type ChatQuestion = { id: string, question: string, options: Array<string>, multiple: boolean, };
export type ChatAttachment = { id: string, name: string, mimeType: string, path: string, };
export type ChatAttachmentInput = { name: string, mimeType: string, base64: string, };
export type ChatAttachmentData = { mimeType: string, base64: string, };
export type ChatItemKind = "user_message" | "assistant_message" | "reasoning" | "proposed_plan" | "command_execution" | "file_change" | "tool" | "approval_request" | "user_input_request" | "system_notice" | "error";
export type ChatItemStatus = "pending" | "running" | "waiting" | "completed" | "failed" | "cancelled" | "interrupted";
export type ChatRunState = "starting" | "ready" | "running" | "waiting" | "switching" | "stopped" | "error";
export type ChatPermissionMode = "approval-required" | "auto-accept-edits" | "auto" | "plan" | "full-access";
export type AgentModel = { id: string, name: string, provider: string | null, description: string | null, isDefault: boolean, };
export type ClaudeAuthMethod = "subscription" | "apiKey" | "oauthToken" | "external" | "unknown";
export type ClaudeAccount = {
/**
 * Scope for subscription limits, when Claude identified the account.
 */
id: string | null, method: ClaudeAuthMethod, verified: boolean, email: string | null, organization: string | null, plan: string | null, provider: string | null, usageBased: boolean, };
export type ClaudeLogin = { "state": "active", account: ClaudeAccount, } | { "state": "signedOut" } | { "state": "unavailable" };
export type ClaudeUsageWindow = { label: string, usedPercent: number, resetsAt: number | null,
/**
 * Claude's own text, when it did not provide an absolute timestamp.
 */
resetText: string | null, };
export type ClaudeUsage = { windows: Array<ClaudeUsageWindow>, noLimits: boolean, refreshedAt: number, };
export type CodexAccount = { id: string, email: string | null, planType: string | null, authMode: string, subscriptionUntil: string | null, importedAt: number, lastUsedAt: number | null, active: boolean, };
export type CodexQuotaWindow = {
/**
 * `fiveHour`, `sevenDay`, or `other`.
 */
kind: string, label: string, usedPercent: number, windowMinutes: number | null,
/**
 * Unix milliseconds (normalized at the Codex boundary).
 */
resetsAt: number | null, };
export type CodexCredits = { hasCredits: boolean, unlimited: boolean, balance: string | null, };
export type CodexResetCard = { id: string, status: string, resetType: string,
/**
 * Unix milliseconds (normalized at the Codex boundary).
 */
grantedAt: number | null,
/**
 * Unix milliseconds (normalized at the Codex boundary).
 */
expiresAt: number | null, title: string | null, description: string | null, };
export type CodexQuota = { planType: string | null, ordinaryUsageAllowed: boolean | null, spendControlReached: boolean | null, rateLimitReachedType: string | null, windows: Array<CodexQuotaWindow>, credits: CodexCredits | null, resetCreditCount: number, resetCredits: Array<CodexResetCard>, refreshedAt: number, error: string | null, };
export type CodexAccounts = { accounts: Array<CodexAccount>, activeAccountId: string | null, quota: CodexQuota | null, };
export type CodexResetResult = { outcome: string, message: string, quota: CodexQuota | null, };
export type ToolAction = "read" | "edit" | "command" | "code-search" | "web-search" | "plan" | "agent" | "other";
export type ApprovalDecisionView = {
/**
 * `accept`, `decline`, `cancel`, `acceptForSession` or `acceptAlways`.
 */
decision: string, label: string, warning: string | null, };
export type ApprovalView = {
/**
 * `command`, `file-read`, `file-change` or another normalized request kind.
 */
kind: string, decisions: Array<ApprovalDecisionView>, };
export type ChangeOrigin = "provider" | "workspace";
export type ChangeKind = "added" | "modified" | "deleted" | "unknown";
export type FileChangeView = {
/**
 * Workspace-relative, validated to stay inside the workspace.
 */
path: string, origin: ChangeOrigin, kind: ChangeKind, additions: number | null, deletions: number | null,
/**
 * Unified diff text, when known.
 */
patch: string | null, };
export type FsEntryKind = "file" | "dir" | "symlink";
export type FsEntry = { name: string, path: string, kind: FsEntryKind, size: number,
/**
 * Matched by `.gitignore` (shown dimmed, like an editor's explorer).
 */
ignored: boolean, };
export type FsListing = { path: string, entries: Array<FsEntry>,
/**
 * More entries existed than the listing returns.
 */
truncated: boolean, };
export type FsContentKind = "text" | "image" | "binary" | "tooLarge";
export type FsFile = { path: string, kind: FsContentKind, size: number,
/**
 * Unix milliseconds on the guest's clock.
 */
modifiedAt: number | null, text: string | null, base64: string | null, mimeType: string | null, };
export type GitFileStatus = { path: string, origPath: string | null, status: string, additions: number | null, deletions: number | null, };
export type GitStatus = {
/**
 * False when the root is not inside a git work tree.
 */
repository: boolean, branch: string | null, upstream: string | null, ahead: number, behind: number,
/**
 * Short id of HEAD; none on an unborn branch.
 */
head: string | null, conflicted: Array<GitFileStatus>, staged: Array<GitFileStatus>, unstaged: Array<GitFileStatus>, untracked: Array<GitFileStatus>, truncated: boolean, };
export type GitFileDiff = { path: string, staged: boolean,
/**
 * Unified diff; empty when the change has no text (e.g. mode only).
 */
patch: string, binary: boolean, truncated: boolean, };
export type GitCommitResult = { head: string, summary: string, };
export type ListeningPort = { port: number,
/**
 * "127.0.0.1", "::1", "0.0.0.0" or "::".
 */
address: string, process: string | null, command: string | null, pid: number | null, };
export type AgentChatCapabilities = { agent: AgentKind,
/**
 * Empty: the permission picker is hidden for this agent.
 */
permissionModes: Array<ChatPermissionMode>, };
export type ChatPromptCapabilities = { images: boolean, audio: boolean, embeddedContext: boolean, };
export type ChatRuntimeCapabilities = { prompt: ChatPromptCapabilities, steer: boolean, resume: boolean, load: boolean, fork: boolean, list: boolean, plans: boolean, subagents: boolean, asyncTasks: boolean, notices: boolean, compaction: boolean, fileRead: boolean, fileWrite: boolean, terminals: boolean,
/**
 * The agent advertised the provider-neutral goal extension.
 */
goal: boolean, };
export type ChatUsage = { input: number, cached: number, cacheWrite: number, output: number, reasoning: number,
/**
 * Time the model spent streaming text or reasoning (for tokens/s).
 */
generationMs: number,
/**
 * Cost in millionths of a US dollar, when the agent reports it.
 */
costMicros: number | null,
/**
 * Prompt size of the latest request, and the model's window.
 */
contextTokens: number | null, contextWindow: number | null, };
export type ChatTurnUsage = { turnId: string, usage: ChatUsage, };
export type UsagePrice = { model: string, input: number, output: number, cacheRead: number, cacheWrite: number, };
export type UsageFilter = { fromMs: number | null, toMs: number | null, agent: string | null, model: string | null, limit: number, offset: number, hostZone: string | null, clockOffset: number, };
export type UsageCounts = { requests: number, input: number, output: number, cacheRead: number, cacheWrite: number, cost: number, unpricedRequests: number, };
export type UsageLog = { id: string, agent: string, sessionId: string | null, model: string, provider: string | null,
/**
 * Unix milliseconds.
 */
timestamp: number, input: number, output: number, cacheRead: number, cacheWrite: number, cost: number | null, durationMs: number | null, status: string | null, };
export type UsageDaily = { timestamp: number, requests: number, input: number, output: number, cacheRead: number, cacheWrite: number, cost: number, unpricedRequests: number, };
export type UsageModel = { model: string, requests: number, input: number, output: number, cacheRead: number, cacheWrite: number, cost: number, unpricedRequests: number, };
export type UsageProvider = { provider: string, requests: number, input: number, output: number, cacheRead: number, cacheWrite: number, cost: number, unpricedRequests: number, };
export type UsageAgent = { agent: string, requests: number, input: number, output: number, cacheRead: number, cacheWrite: number, cost: number, unpricedRequests: number, };
export type PriceSourceStatus = { source: string, lastSuccess: number | null, lastAttempt: number | null, error: string | null, count: number, };
export type UsageDashboard = { summary: UsageCounts, daily: Array<UsageDaily>, models: Array<UsageModel>, providers: Array<UsageProvider>,
/**
 * Per agent, under the same filter.
 */
agents: Array<UsageAgent>, logs: Array<UsageLog>, total: number, prices: Array<UsagePrice>, priceSources: Array<PriceSourceStatus>, };
export type UsageSyncResult = { imported: number, errors: Array<string>, };
export type ChatItem = { id: string, ordinal: number, revision: number,
/**
 * The user message that opened this turn. The UI groups a conversation by
 * this stable id instead of re-splitting a flat item stream.
 */
turnId: string, kind: ChatItemKind, status: ChatItemStatus, title: string, text: string, detail: string, requestId: string | null, questions: Array<ChatQuestion>, attachments: Array<ChatAttachment>,
/**
 * First and latest write, in Unix milliseconds. The store stamps these
 * and `chat_view::present` fills `action`/`subject`/`files`; the bridge
 * leaves all five out of the items it emits.
 */
startedAt: number, updatedAt: number,
/**
 * What a tool-like item did, for grouping ("Read 3 files"). Set by
 * `chat_view::present`; `None` for messages and cards.
 */
action: ToolAction | null,
/**
 * One-line description: the command, the file, the query.
 */
subject: string,
/**
 * Files a change touched, workspace-relative when possible.
 */
files: Array<string>,
/**
 * For approval items: the decisions the guest runtime allows (already
 * filtered by the agent's capability descriptor). The UI renders exactly these.
 */
approval: ApprovalView | null,
/**
 * Structured file changes with their origin (provider-reported or workspace diff).
 */
changes: Array<FileChangeView>,
/**
 * A subagent task this item launches or reports. Set by the ACP view when
 * the provider's tool call names its child session.
 */
taskId: string | null, };
export type PlanReviewKind = "native_request" | "next_turn";
export type PlanReviewStatus = "draft" | "awaiting_review" | "submitting" | "approved" | "changes_requested" | "deferred" | "cancelled" | "expired" | "superseded";
export type PlanExecutionStatus = "not_started" | "starting" | "running" | "completed" | "failed" | "unknown";
export type PlanDecision = "approve" | "request_changes" | "defer";
export type PlanReview = { planId: string, agent: AgentKind, sessionId: string, revision: number, title: string, content: string, contentHash: string, complete: boolean, reviewKind: PlanReviewKind, nativePlanId: string | null, execPermission: ChatPermissionMode, status: PlanReviewStatus, execution: PlanExecutionStatus, decision: PlanDecision | null, feedback: string | null, decisionIntentAt: number | null, appliedAt: number | null, createdAt: number, updatedAt: number, supersededBy: number | null, };
export type PlanRespondOutcome = "applied" | "conflict";
export type PlanRespondResult = { outcome: PlanRespondOutcome, review: PlanReview, };
export type SubagentState = "active" | "completed" | "failed" | "stopped";
export type SubagentTask = { id: string, label: string, subagentType: string | null, detail: string, state: SubagentState, origin: string, startedAt: number, updatedAt: number, items: Array<ChatItem>, };
export type ChatState = { state: ChatRunState, message: string | null, };
export type AcpRuntime = { agent: AgentKind,
/**
 * npm package, e.g. `@agentclientprotocol/claude-agent-acp`.
 */
package: string, license: string,
/**
 * The version this app was tested with; used until the user picks one.
 */
defaultVersion: string,
/**
 * The version new chats run on.
 */
selected: string,
/**
 * Versions present on disk, newest first.
 */
installed: Array<string>,
/**
 * Set when an environment variable overrides the managed install.
 */
overridePath: string | null,
/**
 * The adapter drives the agent CLI from Setup (`false`: the package is the agent).
 */
usesAgentCli: boolean, };
export type ChatConfigChoice = { value: string, name: string, description: string | null, group: string | null, };
export type ChatConfigOption = { id: string, name: string, description: string | null,
/**
 * `model`, `mode`, `thought_level` or another agent-defined category.
 */
category: string | null,
/**
 * `select` or `boolean`.
 */
kind: string,
/**
 * The current value id; `"true"`/`"false"` for a boolean.
 */
current: string, choices: Array<ChatConfigChoice>,
/**
 * Owned by the permission picker (it sets this option), so not shown on its own.
 */
hidden: boolean, };
export type ChatCommand = { name: string, description: string,
/**
 * Placeholder for the command's argument, when it takes one.
 */
hint: string | null, };
export type ChatPlanEntry = { content: string,
/**
 * `high`, `medium` or `low`.
 */
priority: string,
/**
 * `pending`, `in_progress` or `completed`.
 */
status: string, };
export type ChatContextUse = { used: number, size: number,
/**
 * Session cost so far in millionths of `currency`.
 */
costMicros: number | null, currency: string | null, };
export type ChatGoal = { objective: string,
/**
 * `active`, `paused`, `blocked`, `limited`, `complete`, …
 */
status: string, tokenBudget: number | null, tokensUsed: number | null, timeUsedSeconds: number | null,
/**
 * Goal-control actions this adapter actually accepts.
 */
actions: Array<string>, };
export type ChatControls = { agentName: string | null, agentVersion: string | null,
/**
 * Selectors the agent exposes (model, mode, thought level, …), in its order.
 */
options: Array<ChatConfigOption>,
/**
 * Slash commands the agent accepts at the start of a message.
 */
commands: Array<ChatCommand>,
/**
 * The agent's current execution plan (ACP `plan`), replaced whole on each update.
 */
plan: Array<ChatPlanEntry>, context: ChatContextUse | null, title: string | null,
/**
 * Follow-up prompts the agent suggests after a turn.
 */
suggestions: Array<string>,
/**
 * The agent accepts a message while a turn runs (`chat_steer`).
 */
canSteer: boolean,
/**
 * The agent accepts image attachments.
 */
images: boolean,
/**
 * Negotiated protocol capabilities for this live session.
 */
capabilities: ChatRuntimeCapabilities,
/**
 * The provider-neutral goal snapshot, when the live adapter advertises it.
 */
goal: ChatGoal | null, };
export type ChatAsyncTask = { id: string, name: string, taskType: string, description: string,
/**
 * `running`, `paused`, `completed`, `failed` or `stopped`.
 */
state: string, canStop: boolean, summary: string | null, lastToolName: string | null, totalTokens: number | null, toolUses: number | null, durationMs: number | null, outputFilePath: string | null, toolCallId: string | null,
/**
 * The transcript item (tool card) that started this task, so the UI can
 * mark that card and jump to it without knowing how ids are built.
 */
itemId: string | null, startedAt: number, updatedAt: number, };
export type ChatSnapshot = { items: Array<ChatItem>, plans: Array<PlanReview>, subagents: Array<SubagentTask>,
/**
 * In-flight background work reported by the agent's ACP extensions.
 * Unlike transcript blocks this is live state; it is not persisted.
 */
tasks: Array<ChatAsyncTask>, state: ChatState, model: string | null, permission: ChatPermissionMode,
/**
 * Token usage per turn, in turn order.
 */
usage: Array<ChatTurnUsage>,
/**
 * What the live agent offers this session; empty when it is not running.
 */
controls: ChatControls, };
export type WorktreeRefs = { head: string, branches: Array<string>, };
export type SessionWorktree = { path: string, root: string, repository: string, base: string, };
export type Session = { id: string, workspaceId: string, name: string, worktree: SessionWorktree | null,
/**
 * The agent CLI this session runs; `None` for a plain terminal (the
 * user's login shell in the workspace folder).
 */
agent: AgentKind | null, mode: SessionMode, archived: boolean, starred: boolean, createdAt: number, updatedAt: number, lastStartedAt: number | null, status: SessionStatus, };
export type OperationState = "pending" | "running" | "succeeded" | "failed" | "cancelled" | "rolledBack";
export type StepState = "pending" | "running" | "succeeded" | "failed" | "rolledBack" | "rollbackFailed";
export type StepRecord = { name: string, state: StepState, log: Array<string>, };
export type OperationRecord = { id: string, kind: string, title: string, state: OperationState, steps: Array<StepRecord>, error: string | null, retryable: boolean, createdAt: number, updatedAt: number, };
export type EnvironmentSpec = { timezone: string, language: string, };
export type EnvironmentState = {
/**
 * The last successfully applied environment.
 */
committed: EnvironmentSpec, appliedAt: number | null,
/**
 * What the operating system reports right now.
 */
systemTimezone: string | null, systemLanguage: string | null, };
export type FieldState = { field: string, enabled: boolean, reason: string | null, suggestion: string | null, };
export type EnvironmentReview = { fields: Array<FieldState>, changes: Array<string>, blocked: string | null, timezones: Array<string>, languages: Array<string>, };
export type ProxySource = { id: string, name: string, hasUrl: boolean, nodeCount: number, updatedAt: number, };
export type ProxyNode = { id: string, name: string, sourceId: string, sourceName: string, protocol: string,
/**
 * Opaque identity of this node's current configuration. The route id can
 * be reused when a subscription changes.
 */
configRevision: string, };
export type ProxyGroup = { id: string, name: string, nodeIds: Array<string>, };
export type ProxyCatalog = { sources: Array<ProxySource>, nodes: Array<ProxyNode>, groups: Array<ProxyGroup>, };
export type ProxyTestKind = "latency" | "download";
export type ProxyTestResult = { nodeId: string, configRevision: string, kind: ProxyTestKind, testedAt: number, ok: boolean, latencyMs: number | null, bytes: number | null, elapsedMs: number | null, detail: string, };
export type NetworkRoute = { enabled: boolean, chain: Array<string>,
/**
 * Host interface for direct traffic or the first proxy hop; None uses the system route.
 */
bindInterface: string | null, };
export type NetworkStatus = { available: boolean, route: NetworkRoute, running: boolean, error: string | null,
/**
 * The latest gate transition or verification failure. Kept separate from
 * `error`, which describes the gateway process itself.
 */
gateError: string | null, gate: NetworkGateMode, connectivity: NetworkConnectivity, failedChecks: number, connectivityError: string | null, gatewaySession: number | null, paused: boolean, autoRecover: boolean,
/**
 * When a check fails only because the machine's time zone differs from
 * the verified exit, the Mac sets the machine to the exit's zone and
 * checks again.
 */
regionFollowsExit: boolean,
/**
 * Stable identity of the route and guest environment this grant covers.
 */
revision: string,
/**
 * Monotonic in-process token used to discard stale checks.
 */
generation: number, checkedAt: number | null, expiresAt: number | null, nextRetryAt: number | null, retryAttempt: number, report: NetworkCheckReport | null, };
export type NetworkGateMode = "closed" | "checking" | "open";
export type NetworkConnectivity = "normal" | "rechecking" | "blocked";
export type NetworkTraffic = { gatewaySession: number, sampledAt: number, rxBytes: number, txBytes: number, rxBps: number | null, txBps: number | null, };
export type NetworkHistoryEntry = { id: number, at: number, kind: string, message: string, failedChecks: number, report: NetworkCheckReport | null, };
export type IpType = "mobile" | "residential" | "commercial" | "server" | "unknown";
export type IpClassification = { ipType: IpType, inferred: boolean, organization: string | null, asn: number | null, source: string, };
export type NetworkCheckStatus = "pass" | "fail" | "unknown" | "skipped";
export type NetworkCheckCategory = "route" | "gateway" | "dns" | "transport" | "boundary" | "exit" | "environment";
export type NetworkEvidence = { label: string, value: string, source: string, observedAt: number | null, };
export type NetworkCheckItem = { id: string, title: string, category: NetworkCheckCategory, status: NetworkCheckStatus, required: boolean, summary: string, evidence: Array<NetworkEvidence>, };
export type NetworkCheckReport = { id: string, revision: string, startedAt: number, finishedAt: number, checks: Array<NetworkCheckItem>, passedRequired: number, totalRequired: number, canOpen: boolean, };
export type GuestNetworkProbe = { checkedAt: number, hostname: string | null, username: string | null, timezone: string | null, language: string | null, dnsOk: boolean, dnsMs: number | null, connectOk: boolean, connectMs: number | null, httpsOk: boolean, httpsMs: number | null, publicIp: string | null, traceLocation: string | null, ipv6Blocked: boolean, privateBlocked: boolean,
/**
 * Only applicable while unrestricted public traffic is closed.
 */
unlistedBlocked: boolean | null, udpBlocked: boolean, error: string | null, };
export type BrowserEnvironmentState = { timezone: string | null, language: string | null, languages: Array<string>, platform: string | null, };
export type BrowserEnvironmentProbe = { enabled: boolean, expected: BrowserEnvironmentState,
/**
 * The installed profile in the guest; `None` when it could not be read.
 */
installed: BrowserEnvironmentState | null, live: BrowserLive, observed: BrowserEnvironmentState | null,
/**
 * Why the live browser has no observation, in a few words.
 */
liveDetail: string | null,
/**
 * A problem with the installed settings (blocks: they must be right).
 */
error: string | null, };
export type BrowserLive = "observed" | "notRunning" | "idle" | "unavailable";
export type BrowserSettings = { followMachine: boolean, timezone: string, locale: string, userAgent: string, platform: string, hardwareConcurrency: number, deviceMemory: number, };
export type BrowserEnvironmentView = { enabled: boolean, machineRunning: boolean, settings: BrowserSettings | null, probe: BrowserEnvironmentProbe | null,
/**
 * Whether the live pages use the installed settings; `None` when no
 * live page was observed. Judged on the Mac, like the network check.
 */
liveMatches: boolean | null,
/**
 * Unix milliseconds of `probe`.
 */
checkedAt: number | null, };
export type GeoInfo = { ip: string, countryCode: string | null, country: string | null, region: string | null, city: string | null,
/**
 * Provider (ISP or organisation), as the service names it.
 */
isp: string | null, ipType: IpType | null, asn: number | null, latitude: number | null, longitude: number | null, timezone: string | null,
/**
 * The service that answered.
 */
source: string, checkedAt: number, };
export type HopCheck = { nodeId: string, ok: boolean, latencyMs: number | null, detail: string, };
export type RouteCheck = { hops: Array<HopCheck>, brokenAt: number | null, checkedAt: number, };
export type NodePlace = { nodeId: string, host: string, geo: GeoInfo | null, error: string | null, };
export type ProbeResult = { ok: boolean, latencyMs: number | null, detail: string, };
export type AgentStatus = { kind: AgentKind, name: string, package: string, requiresNode: boolean, installed: boolean, binary: string | null,
/**
 * Version and authentication checks ran for this installation.
 */
cliChecked: boolean, version: string | null, configPath: string, configExists: boolean, configError: string | null, modelKey: string, model: string | null, authenticated: boolean, authDetail: string, claudeLogin: ClaudeLogin | null, };
export type AgentConfigText = { kind: AgentKind, path: string, exists: boolean, text: string, };
export type McpTransport = { "type": "stdio", command: string, args: Array<string>, env: { [key in string]: string }, } | { "type": "http", url: string, headers: { [key in string]: string }, sse: boolean, };
export type McpServer = { name: string, transport: McpTransport, enabled: boolean, };
export type McpLibrary = { rows: Array<McpRow>, agents: Array<McpAgent>, };
export type McpAgent = { kind: AgentKind, path: string | null, error: string | null, supportsSse: boolean, };
export type McpRow = { name: string, endpoint: string, transport: string, description: string, managed: boolean, conflict: boolean, bindings: Array<McpBinding>, };
export type McpBinding = { kind: AgentKind, state: McpBindingState, wanted: boolean, };
export type McpBindingState = "enabled" | "disabled" | "unreadable" | "conflict" | "unsupported";
export type McpChange = { "type": "import" } | { "type": "save", servers: Array<McpServer>, targets: Array<AgentKind>, replace: boolean, description: string, } | { "type": "bind", names: Array<string>, targets: Array<AgentKind>, enabled: boolean, } | { "type": "remove", names: Array<string>, } | { "type": "sync" };
export type SkillRepository = { repository: string, branch: string, directory: string, };
export type SkillOrigin = { repository: SkillRepository, path: string, revision: string, };
export type DiscoveredSkill = { name: string, folder: string, description: string, origin: SkillOrigin, };
export type RegistrySkill = { id: string, skillId: string, name: string, source: string, installs: number, };
export type SkillRegistry = { skills: Array<RegistrySkill>, count: number, };
export type SkillMode = "link" | "copy";
export type ManagedSkill = { folder: string, name: string, description: string, origin: SkillOrigin | null, importedFrom: string | null, digest: string, mode: SkillMode, targets: Array<AgentKind>, };
export type SkillLibrary = { installed: Array<InstalledSkill>, local: Array<LocalSkill>, backups: Array<SkillBackup>, repositories: Array<SkillRepository>, };
export type InstalledSkill = { skill: ManagedSkill, bindings: Array<SkillBinding>, };
export type SkillBinding = { kind: AgentKind, state: string, wanted: boolean, };
export type LocalSkill = { kind: AgentKind, entry: SkillEntry, };
export type SkillBackup = { id: string, folder: string, name: string, at: bigint, };
export type SkillInstall = { "type": "repository", origin: SkillOrigin, } | { "type": "registry", repository: string, skillId: string, } | { "type": "zip", contents: string, } | { "type": "local", kind: AgentKind, path: string, } | { "type": "update", folder: string, };
export type SkillChange = { "type": "bind", folders: Array<string>, targets: Array<AgentKind>, enabled: boolean, } | { "type": "remove", folders: Array<string>, } | { "type": "restore", id: string, } | { "type": "deleteBackup", id: string, } | { "type": "sync" };
export type SkillEntry = { name: string, description: string,
/**
 * The skill's folder.
 */
path: string,
/**
 * `user` (the agent's home folder) or `project` (the workspace).
 */
scope: string, };
export type SkillFile = { path: string, name: string, description: string, body: string, scope: string, };
export type AgentExtensions = { kind: AgentKind,
/**
 * The file holding the agent's own MCP servers; `None` when the agent
 * has no native MCP support.
 */
mcpPath: string | null,
/**
 * Whether a server can be switched off without removing it.
 */
mcpCanDisable: boolean, mcpServers: Array<McpServer>,
/**
 * Why the MCP file could not be read (it is then left untouched).
 */
mcpError: string | null, skillsPath: string, skills: Array<SkillEntry>, };
export type AgentCommand = { name: string, description: string | null,
/**
 * Argument hint, e.g. `<file>`.
 */
hint: string | null,
/**
 * `command` (typed with `/`) or `skill` (typed with `$`).
 */
kind: string,
/**
 * Where it comes from: `builtin`, `user`, `project`, `extension`, …
 */
source: string | null,
/**
 * Text the composer inserts, e.g. `/review ` or `$pdf `.
 */
insert: string, };
export type McpStatus = { name: string,
/**
 * `connected`, `failed`, `disabled`, `needsAuth`, `pending` or the
 * agent's own word.
 */
status: string, detail: string | null, tools: number | null, };
export type AgentCatalog = { commands: Array<AgentCommand>, mcp: Array<McpStatus>,
/**
 * Set when the agent could not be asked; the lists are then empty.
 */
error: string | null, };
export type BackupEntry = { path: string, name: string, createdAt: number, size: number, };
export type HealthReport = { ok: boolean, lines: Array<string>, };
export type CheckStatus = "ok" | "warning" | "missing";
export type SetupAction = { "kind": "installBasePackages" } | { "kind": "installNode" } | { "kind": "installPython" } | { "kind": "installAgent", agent: AgentKind, };
export type SetupCheck = { id: string, label: string, status: CheckStatus, detail: string, action: SetupAction | null, };
export type NodeSource = "managed" | "system" | "other";
export type NodeRuntime = { version: string, npm: string | null, path: string, source: NodeSource, };
export type NodeInstalled = { version: string,
/**
 * True when this is the version Cabin links as `node`.
 */
current: boolean, path: string, };
export type NodeState = {
/**
 * The `node` sessions and installs actually run.
 */
active: NodeRuntime | null,
/**
 * Versions Cabin installed, newest first.
 */
installed: Array<NodeInstalled>,
/**
 * Ubuntu's apt Node.js, if present.
 */
system: NodeRuntime | null, minimumMajor: number, recommendedMajor: number,
/**
 * Download mirror used last (or the default).
 */
mirror: string, installDir: string,
/**
 * Architecture tag of the builds this machine needs (`linux-arm64`).
 */
platform: string, };
export type NodeRelease = { version: string, major: number,
/**
 * LTS codename, or null for a Current release.
 */
lts: string | null, date: string, npm: string | null, security: boolean, installed: boolean, };
export type GuestClock = { nowMs: number, timezone: string | null, };
export type GuestMachineObservation = { at: number, kernel: string | null, uptimeSeconds: number, rootTotalBytes: number | null, rootAvailableBytes: number | null, };
export type MonitorSample = { at: number, cpuPercent: number | null, memoryUsed: number | null, memoryTotal: number | null, latencyMs: number | null, downloadBps: number | null, uploadBps: number | null, diskReadBps: number | null, diskWriteBps: number | null, swapUsed: number | null,
/**
 * `None` when the machine has no swap.
 */
swapTotal: number | null,
/**
 * Load averages over 1, 5 and 15 minutes.
 */
load: LoadAverage | null, cpuCount: number | null, };
export type LoadAverage = { one: number, five: number, fifteen: number, };
export type ProcessInfo = { pid: number, name: string,
/**
 * The command line, shortened; empty for kernel threads.
 */
command: string,
/**
 * Share of the whole machine (all cores), 0–100.
 */
cpuPercent: number, memory: number,
/**
 * Seconds since the process started.
 */
runTime: number, };
export type ProcessList = { at: number, total: number, byCpu: Array<ProcessInfo>, byMemory: Array<ProcessInfo>, };
export type AppSettings = {
/**
 * `host:port` used for TCP-connect latency samples.
 */
latencyTarget: string,
/**
 * `host:port` used to check whether the network works directly.
 */
checkTarget: string, defaultAgent: AgentKind, };
export type AppSnapshot = { workspaces: Array<Workspace>, sessions: Array<Session>, agents: Array<AgentStatus>, environment: EnvironmentState,
/**
 * The node catalog and the route are the Mac's; it fills these in.
 */
proxy: ProxyCatalog, network: NetworkStatus, operations: Array<OperationRecord>, settings: AppSettings, dataDir: string, hostname: string, };
export type AppEvent = { "type": "operation", record: OperationRecord, } | { "type": "sessionOutput", id: string, generation: number, offset: number, data: string, } | { "type": "terminalStatus", id: string, generation: number, status: SessionStatus, } | { "type": "sessionStatus", id: string, status: SessionStatus, } | { "type": "chatItem", id: string, item: ChatItem, } | { "type": "chatPlan", id: string, plan: PlanReview, } | { "type": "chatSubagent", id: string, task: SubagentTask, } | { "type": "chatTask", id: string, task: ChatAsyncTask, } | { "type": "chatState", id: string, state: ChatState, } | { "type": "chatModel", id: string, model: string | null, } | { "type": "chatPermission", id: string, permission: ChatPermissionMode, } | { "type": "chatUsage", id: string, turn: ChatTurnUsage, } | { "type": "chatControls", id: string, controls: ChatControls, } | { "type": "monitor", sample: MonitorSample, } | { "type": "agents", agents: Array<AgentStatus>, } | { "type": "network", catalog: ProxyCatalog, status: NetworkStatus, } | { "type": "environment", state: EnvironmentState, } | { "type": "library", workspaces: Array<Workspace>, sessions: Array<Session>, } | { "type": "machine", status: MachineStatus, } | { "type": "snapshot", snapshot: AppSnapshot, } | { "type": "install", steps: Array<InstallStep>, } | { "type": "start", report: StartReport, } | { "type": "sync", status: SyncStatus, };
export type MachineState = "notInstalled" | "stopped" | "running";
export type GuestConnection = "offline" | "connecting" | "updating" | "connected";
export type MachineStatus = { state: MachineState, connection: GuestConnection,
/**
 * What the Mac is doing to the VM right now ("Installing…", "Stopping…").
 */
busy: string | null, sshPort: number | null, cpus: number | null, memoryMb: number | null, diskSize: string | null,
/**
 * The in-app desktop (headless Sway over VNC) is installed.
 */
display: boolean,
/**
 * Guest operating system id (`os::RELEASES`), once installed.
 */
os: string | null, browser: boolean, device: boolean, guestVersion: string | null,
/**
 * The VM's Linux user and hostname (from the install).
 */
username: string | null, hostname: string | null, error: string | null, };
export type MachineInstallOptions = { cpus: number | null, memoryMb: number | null, diskSize: string | null, username: string | null, hostname: string | null, device: boolean,
/**
 * Archive mirror for apt in the VM, matching the OS (`None`: the
 * image's default archive).
 */
aptMirror: string | null,
/**
 * Guest operating system id (`None`: the default, Ubuntu 24.04).
 */
os: string | null, };
export type StartFact = { label: string, value: string, };
export type StartPhaseState = "pending" | "running" | "done" | "skipped" | "failed";
export type StartPhase = { id: string, title: string, state: StartPhaseState, startedAt: number | null, finishedAt: number | null, detail: string | null, };
export type StartState = "running" | "succeeded" | "failed";
export type StartReport = { state: StartState, startedAt: number, finishedAt: number | null, error: string | null,
/**
 * QEMU path and version, accelerator, firmware, resources, ports, pid…
 */
facts: Array<StartFact>, phases: Array<StartPhase>, logPath: string | null, logTail: Array<string>,
/**
 * The guest's serial console (only output from this boot).
 */
serialPath: string | null, serialTail: Array<string>, };
export type HostSnapshot = { machine: MachineStatus, guest: AppSnapshot | null,
/**
 * The running or most recent machine start.
 */
start: StartReport | null, };
export type DownloadProgress = { doneBytes: number, totalBytes: number | null, bytesPerSec: number | null,
/**
 * All bytes are here and the checksum is being verified.
 */
verifying: boolean, };
export type InstallStep = { id: string, title: string,
/**
 * pending | running | done | cached | failed | rolledBack
 */
state: string, detail: string | null, logTail: Array<string>, logPath: string | null, seconds: number | null,
/**
 * Unix milliseconds.
 */
appliedAt: number | null, fingerprint: string | null,
/**
 * Bytes, total and speed while the step downloads (the cloud image).
 */
download: DownloadProgress | null, };
export type InstallPlan = { os: string, osName: string, osChoices: Array<OsChoice>,
/**
 * Where the newest build is found; it is pinned when the install starts.
 */
imageIndex: string, qemuRelease: string, qemuInstalled: boolean, cpus: number, memoryMb: number, diskSize: string, network: string, accel: string, firmware: string | null, diskPath: string, seedPath: string, username: string, hostname: string, sshPort: number, aptMirror: string | null, steps: Array<InstallStep>, };
export type MirrorResult = { uri: string, host: string,
/**
 * The archive the image uses when no mirror is chosen.
 */
default: boolean,
/**
 * ok | noArm64 | unreachable
 */
status: string, latencyMs: number | null, speedKbps: number | null, detail: string, };
export type OsChoice = { id: string, name: string,
/**
 * A development release with daily images.
 */
testing: boolean,
/**
 * Archive mirrors offered for it, the default first.
 */
mirrors: Array<string>, };
export type SyncMode = "mirror" | "safe" | "twoWay";
export type GitHandling = "copyOnce" | "sync" | "skip";
export type SyncRules = {
/**
 * Honour `.gitignore` files (and `.git/info/exclude`).
 */
gitignore: boolean,
/**
 * Extra gitignore-style patterns, e.g. `*.log` or `build/`.
 */
ignore: Array<string>,
/**
 * Relative paths the user unticked; each excludes itself and below.
 */
excluded: Array<string>,
/**
 * How `.git` takes part (only `Sync` lets the regular scan see it).
 */
git: GitHandling, };
export type SyncProject = { id: string,
/**
 * The workspace whose folder is the machine side.
 */
workspaceId: string | null, name: string, hostPath: string, guestPath: string, mode: SyncMode, rules: SyncRules, paused: boolean,
/**
 * `.git` was copied into the machine (GitHandling::CopyOnce).
 */
gitCopied: boolean, createdAt: number, };
export type SyncProjectInput = { workspaceId: string | null, name: string, hostPath: string, guestPath: string, mode: SyncMode, rules: SyncRules, };
export type SyncState = "offline" | "scanning" | "syncing" | "watching" | "paused" | "error";
export type SyncStatus = { id: string, state: SyncState, error: string | null,
/**
 * Unix milliseconds of the last completed cycle.
 */
lastSyncAt: number | null,
/**
 * Paths in step (files, folders and links).
 */
files: number,
/**
 * Totals since the app started.
 */
pushed: number, pulled: number,
/**
 * Paths changed on both sides, waiting for a choice.
 */
conflicts: Array<string>, };
export type SyncView = { project: SyncProject, status: SyncStatus, };
export type SyncPreviewItem = { name: string, dir: boolean,
/**
 * Files that would sync under it (1 for an included file).
 */
files: number, bytes: number, included: boolean,
/**
 * Why it is left out: `unticked`, `ignored` (by .gitignore,
 * .cabinignore or a pattern) or `git` (see GitHandling).
 */
reason: string | null, };
export type SyncPreview = { hostPath: string, items: Array<SyncPreviewItem>, files: number, bytes: number,
/**
 * The folder is a git repository (has `.git`).
 */
git: boolean,
/**
 * The folder has a `.gitignore` at its top.
 */
gitignore: boolean, };
export type WorkspacePatch = { name?: string, path?: string, modelOverrides?: Array<ModelOverride>, archived?: boolean, };
export type SessionPatch = { name?: string, archived?: boolean, starred?: boolean, };
