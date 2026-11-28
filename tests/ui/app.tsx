// The real <App/> over a fake Tauri bridge. `?at=` picks the screen.
import { createRoot } from "react-dom/client";
import type { AgentStatus, Session, Workspace } from "../../src/services/ipc/generated/core";
import type { View } from "../../src/state/ui";

const at = new URLSearchParams(location.search).get("at") ?? "new";
// The smoke assertions use English labels regardless of the runner's locale.
localStorage.setItem("cabin.locale", "en");
const now = Date.now();
const sent: unknown[] = [];
const cliRequests: Record<string, unknown>[] = [];
Object.assign(window, { __sent: sent, __cliRequests: cliRequests });

const codex: AgentStatus = {
  kind: "codex", name: "Codex", package: "@openai/codex", requiresNode: false, installed: true,
  binary: "/usr/bin/codex", version: "0.0.1", cliChecked: true, authenticated: false, authDetail: "Signed out",
  configPath: "/home/dev/.codex/config.toml", configExists: false, configError: null, modelKey: "model", model: null, claudeLogin: null,
};
const defaultWorkspace: Workspace = {
  id: "default", name: "Workspace", path: "/home/dev/workspace", modelOverrides: [], archived: false, createdAt: now, updatedAt: now,
};
const loginSession: Session = {
  id: "login", workspaceId: defaultWorkspace.id, agent: "codex", mode: "login", name: "Codex CLI login", worktree: null,
  archived: false, starred: false, createdAt: now, updatedAt: now, lastStartedAt: now, status: { state: "running", pid: 2 },
};

const item = (id: string, kind: string, over: object) => ({
  id, ordinal: Number(id.slice(1)), revision: 1, turnId: "i1", kind, status: "completed", title: "", text: "", detail: "", requestId: null,
  questions: [], attachments: [], startedAt: now, updatedAt: now, action: null, subject: "", files: [], approval: null, changes: [], taskId: null, ...over,
});
const replies: Record<string, (params: Record<string, unknown>) => unknown> = {
  chat_snapshot: () => ({
    items: [
      item("i1", "user_message", { text: "Build it" }),
      item("i2", "approval_request", { status: "waiting", requestId: "r1", title: "Run a command", text: "npm test", approval: { kind: "command", decisions: [
        { decision: "decline", label: "Decline", warning: null }, { decision: "accept", label: "Approve", warning: null }] } }),
    ],
    plans: [], subagents: [], tasks: [], usage: [], state: { state: "waiting", message: null }, model: "m", permission: "approval-required",
    controls: { agentName: null, agentVersion: null, options: [], commands: [], plan: [], context: null, title: null, suggestions: [], canSteer: false, images: false, goal: null,
      capabilities: { prompt: {} } },
  }),
  chat_respond: (params) => { sent.push(params); return null; },
  agents_refresh: () => at === "agents:login" ? [codex] : [],
  agent_extensions: ({ kind }) => ({ kind, mcpPath: null, mcpCanDisable: false, mcpServers: [], mcpError: null, skillsPath: "", skills: [] }),
  agent_terminal_open: (params) => {
    cliRequests.push(params);
    patchState((state) => ({ ...state, workspaces: [defaultWorkspace], sessions: [loginSession] }));
    return loginSession;
  },
  terminal_snapshot: () => ({ generation: 1, status: loginSession.status, startOffset: 0, endOffset: 0, data: "" }),
  agent_chat_capabilities: () => [],
  acp_runtimes: () => [],
  mcp_library: () => ({ agents: [], rows: [] }),
  skill_library: () => ({ installed: [], local: [], backups: [], repositories: [] }),
  usage_sync: () => ({ imported: 0, errors: [] }),
  usage_dashboard: () => ({
    summary: { requests: 0, input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0, unpricedRequests: 0 },
    daily: [], total: 0, models: [], providers: [], agents: [], logs: [], prices: [], priceSources: [],
  }),
  setup_checks: () => [],
  node_state: () => ({ active: null, minimumMajor: 20, recommendedMajor: 24, installed: [], system: null }),
  codex_accounts: () => ({ accounts: [], activeAccountId: null, quota: null }),
  network_interfaces: () => [],
  mac_permissions: () => [],
  environment_review: () => ({ fields: [], changes: [], blocked: null, timezones: ["UTC"], languages: ["C.UTF-8"] }),
  machine_checks: () => ({ host: { platform: "macos", memoryMb: 16384, cpus: 8, username: "dev", deviceProfileSupported: true, maxDiskGb: 120 }, items: [] }),
  machine_plan: () => ({ os: "ubuntu-24.04", osName: "Ubuntu 24.04 LTS", osChoices: [], username: "dev", hostname: "dev-vm" }),
  machine_test_mirrors: () => [],
};
Object.assign(window, {
  __TAURI_INTERNALS__: {
    transformCallback: () => 1, unregisterCallback() {},
    async invoke(command: string, args: { method?: string; params?: Record<string, unknown> }) {
      const guest = command === "guest_invoke";
      return replies[guest ? args.method! : command]?.(guest ? args.params ?? {} : args as Record<string, unknown>) ?? null;
    },
  },
  __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener() {} },
});

const [{ default: App }, { patchState }, { uiActions }] = await Promise.all([import("../../src/App"), import("../../src/state/store"), import("../../src/state/ui")]);
const install = at === "install", failed = at === "failed", stopped = at === "stopped";
const down = install || failed || stopped;
const session = { id: "s1", workspaceId: "w1", name: "Fix the build", worktree: null, agent: "claude", mode: "ui", archived: false, starred: false, createdAt: now, updatedAt: now, lastStartedAt: now, status: { state: "running", pid: 1 } };
// Only the fields the screens read; the shapes are the backend's.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
patchState((state: any) => ({
  ...state, ready: true, guestReady: !down, networkLoaded: true, dataDir: "/data", agents: at === "agents:login" ? [codex] : [],
  settings: { defaultAgent: "claude", latencyTarget: "1.1.1.1:443", checkTarget: "example.com:443" },
  machine: { ...state.machine, state: install ? "notInstalled" : stopped ? "stopped" : "running", connection: down ? "offline" : "connected", busy: null, username: "dev", hostname: "dev-vm", error: failed ? "SSH did not answer" : null, os: "ubuntu-24.04", guestVersion: "0", cpus: 4, memoryMb: 8192, diskSize: "40G" },
  workspaces: at === "agents:login" ? [] : [{ id: "w1", name: "cabin", path: "/w", modelOverrides: [], archived: false, createdAt: now, updatedAt: now }],
  sessions: at === "agents:login" ? [] : [session],
  start: failed ? { state: "failed", startedAt: now - 9000, finishedAt: now, error: "SSH did not answer", facts: [], logPath: "/l", logTail: Array(40).fill("ssh: retrying"), serialPath: null, serialTail: [], phases: [{ id: "ssh", title: "Connect to the machine", state: "failed", startedAt: now - 9000, finishedAt: now, detail: "SSH did not answer" }] } : state.start,
}));
if (!down) {
  const [kind, tab] = at.split(":");
  uiActions.navigate((kind === "session" ? { kind, id: "s1" } : kind === "settings" ? { kind, tab } : { kind }) as View);
}
createRoot(document.getElementById("root")!).render(<App />);
