import "./chatmock";
import ReactDOM from "react-dom/client";
import { MantineProvider } from "@mantine/core";
import { theme } from "../../src/theme";
import "../../src/App.css";
import { Chat } from "../../src/components/Chat";
import { patchState } from "../../src/state/store";

const q = new URLSearchParams(location.search);
if (q.get("theme")) document.documentElement.dataset.theme = q.get("theme")!;
patchState((s: any) => ({ ...s, ready: true, guestReady: true, machine: { state: "running", connection: "connected", busy: null }, workspaces: [{ id: "w1", name: "cabin", path: "/home/dev/cabin", modelOverrides: [], archived: false, createdAt: 1, updatedAt: 1 }],
  sessions: [{ id: "s1", workspaceId: "w1", name: "Add Tailwind", worktree: null, agent: "claude", mode: "ui", archived: false, starred: false, createdAt: 1, updatedAt: 1, lastStartedAt: 1, status: { state: "running", pid: 1 } }],
  agents: [{ kind: "claude", installed: true, version: "2", cliChecked: true }] }));
ReactDOM.createRoot(document.getElementById("root")!).render(
  <MantineProvider theme={theme} forceColorScheme={(q.get("theme") as any) ?? undefined}>
    <div className="shell" style={{ height: "100vh", gridTemplateColumns: "1fr" }}><main className="main" style={{ gridColumn: 1, margin: 8 }}><Chat sessionId="s1" workspaceId="w1" agent="claude" /></main></div>
  </MantineProvider>);

// ?click=<css>&click=<css>… presses those elements in order (text match: "css|text").
(async () => {
  for (const spec of q.getAll("click")) {
    await new Promise((r) => setTimeout(r, 900));
    const [sel, text] = spec.split("|");
    const el = [...document.querySelectorAll<HTMLElement>(sel)].find((e) => !text || e.textContent?.includes(text));
    el?.click();
  }
})();
