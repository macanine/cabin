import ReactDOM from "react-dom/client";
import App from "../../src/App";
import { UsageMonitor } from "../../src/components/usage/UsageMonitor";
import { MantineProvider } from "@mantine/core";
import { theme } from "../../src/theme";
import "../../src/App.css";
import { Sidebar } from "../../src/components/Sidebar";
import { MachineGate } from "../../src/components/MachineGate";
import { patchState } from "../../src/state/store";
import { startNetworkMonitor } from "../../src/state/networkMonitor";
import { uiActions, useUi } from "../../src/state/ui";
import { TitleBar, TitleBarSlotProvider, WindowStrip } from "../../src/components/TitleBar";
import { NewSessionView } from "../../src/views/NewSessionView";
import { UsageView } from "../../src/views/UsageView";
import { SettingsView } from "../../src/views/SettingsView";
import { McpView } from "../../src/views/McpView";
import { SkillsView } from "../../src/views/SkillsView";
import { ChromePage } from "../../src/views/ChromeView";
import { AgentsView } from "../../src/views/AgentsView";
import { SessionView } from "../../src/views/SessionView";
import { useState } from "react";
import { mockIpc } from "./pagemock";
import { initPerf } from "../../src/lib/perfMode";

const q = new URLSearchParams(location.search);
if (q.get("perf")) localStorage.setItem("cabin.perf", q.get("perf")!);
initPerf();
const v = q.get("v") ?? "new";
if (q.get("theme")) document.documentElement.dataset.theme = q.get("theme")!;
const networkMode = q.get("network");
const mock = v === "session" ? (await import("./chatmock"), null) : mockIpc(networkMode);
const stopped = networkMode === "stopped";
const now = Date.now();
const ws = (id: string, name: string) => ({ id, name, path: "/home/dev/" + name, modelOverrides: [], archived: false, createdAt: now, updatedAt: now });
const se = (id: string, w: string, name: string, agent: any, state: any = { state: "stopped" }, ago = 3600e3) => ({ id, workspaceId: w, name, worktree: null, agent, mode: "ui", archived: false, starred: false, createdAt: now - ago, updatedAt: now - ago, lastStartedAt: now - ago, status: state });
patchState((s: any) => ({ ...s, ready: true, guestReady: !stopped, networkLoaded: true,
  machine: { state: stopped ? "stopped" : "running", connection: stopped ? "offline" : "connected", busy: null, username: "dev", hostname: "dev-vm", error: null, os: "ubuntu-24.04", guestVersion: "0.2.0", cpus: 4, memoryMb: 4096 },
  workspaces: [ws("w1", "cabin"), ws("w2", "notes"), ws("w3", "api-server")],
  sessions: [se("s1", "w1", "Add Tailwind to the UI", "claude", { state: "running", pid: 1 }, 600e3), se("s2", "w1", "Fix sync conflicts", "codex", undefined, 7200e3), se("s3", "w2", "Shell", null, undefined, 86400e3), se("s4", "w3", "Refactor the parser", "pi", undefined, 90000e3), se("s5", "w3", "Write release notes", "opencode", undefined, 200000e3)],
  agents: ["claude", "codex", "opencode", "pi", "antigravity"].map((kind, i) => {
    const requiresNode = kind !== "antigravity";
    const installed = i < 3 || !requiresNode && q.get("agy") === "installed";
    const claudeAccount = kind === "claude" ? {
      id: "claude-team", method: "subscription", verified: true,
      email: "dev@example.com", organization: "Example Studio", plan: "Team", provider: null, usageBased: false,
    } : null;
    const unavailable = kind === "claude" && q.has("claudeUnavailable");
    const claudeLogin = unavailable ? { state: "unavailable" } : claudeAccount ? { state: "active", account: claudeAccount } : null;
    const authDetail = unavailable ? "Claude Code's sign-in status is temporarily unavailable." : claudeAccount ? "Signed in through Claude Code" : "";
    return { kind, name: kind, package: requiresNode ? "@x/" + kind : "agy", requiresNode, installed, binary: "/b", version: installed ? "1.2." + i : null, cliChecked: true, authenticated: i === 0 && !unavailable, authDetail, configError: null, claudeLogin };
  }),
  settings: { defaultAgent: "claude", latencyTarget: "1.1.1.1:443", checkTarget: "example.com:443" }, dataDir: "/home/dev/.local/share/cabin",
  environment: { committed: { timezone: "UTC", language: "en_US.UTF-8" }, appliedAt: null, systemTimezone: "Asia/Shanghai", systemLanguage: "C.UTF-8" },
  network: mock?.network ?? { ...s.network, available: true, running: true, gate: "open", revision: "r", expiresAt: now + 3600e3, report: null },
  start: v === "start" ? {
    state: "running", startedAt: now, finishedAt: null, error: null, facts: [], logPath: null,
    logTail: ["Waiting for SSH"], serialPath: null, serialTail: [],
    phases: [
      { id: "prepare", title: "Check the configuration", state: "done", startedAt: now, finishedAt: now, detail: null },
      { id: "connect", title: "Connect to the machine", state: "running", startedAt: now, finishedAt: null, detail: null },
      { id: "ready", title: "Open the workspace", state: "pending", startedAt: null, finishedAt: null, detail: null },
    ],
  } : s.start,
}));
if (v === "agents") uiActions.navigate({ kind: "agents" });
if (v === "agents" && q.has("claudeSwitch")) {
  window.addEventListener("test:claude-seat", () => patchState((state) => ({
    ...state,
    agents: state.agents.map((agent) => agent.claudeLogin?.state === "active" ? {
      ...agent, claudeLogin: { state: "active", account: { ...agent.claudeLogin.account, id: "claude-personal", organization: null, plan: "Pro" } },
    } : agent),
  })));
}
// Exit and origin lookups run only when asked, so other pages stay deterministic.
if (q.has("monitor")) startNetworkMonitor();
if (v === "permissions") uiActions.navigate({ kind: "settings", tab: "permissions" });

function AgentsPage() {
  const view = useUi((state) => state.view);
  if (view.kind === "session") return <output>Opened session {view.id}</output>;
  return view.kind === "agents" ? <AgentsView agent={view.agent} section={view.section} /> : <AgentsView />;
}

function Page() {
  if (v !== "agents") uiActions.navigate(v === "session" ? { kind: "session", id: "s1" } : { kind: "new" });
  switch (v) {
    case "usage": return <UsageView />;
    case "settings": return <SettingsView tab={(q.get("tab") as never) ?? "general"} />;
    case "mcp": return <McpView agent={undefined} />;
    case "skills": return <SkillsView agent={undefined} />;
    case "chrome": return <ChromePage />;
    case "session": return <SessionView id="s1" />;
    case "start": return <MachineGate />;
    case "agents": return <AgentsPage />;
    default: return <NewSessionView />;
  }
}
function Shell() {
  const [slot, setSlot] = useState<HTMLElement | null>(null);
  return (
    <TitleBarSlotProvider value={slot}>
      <div className="shell" style={{ height: "100vh" }}>
        <WindowStrip focus={false} back={false} />
        <Sidebar />
        <main className="main"><TitleBar slotRef={setSlot} /><div className="flex min-h-0 flex-1 flex-col"><Page /></div></main>
      </div>
    </TitleBarSlotProvider>
  );
}
ReactDOM.createRoot(document.getElementById("root")!).render(v === "permissions" ? <App />
  : <MantineProvider theme={theme} forceColorScheme={(q.get("theme") as never) ?? "light"}><UsageMonitor><Shell /></UsageMonitor></MantineProvider>);
