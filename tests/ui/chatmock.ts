// A fake Tauri bridge with one realistic conversation, for visual work.
const now = Date.now();
let n = 0;
const base = (turnId: string, kind: string, status: string, over: Record<string, unknown>) => ({
  id: `i${++n}`, ordinal: n, revision: 1, turnId, kind, status, title: "", text: "", detail: "", requestId: null, questions: [], attachments: [],
  startedAt: now - 600000 + n * 9000, updatedAt: now - 600000 + n * 9000 + 4000, action: null, subject: "", files: [], approval: null, changes: [], taskId: null, ...over,
});
const patchA = `--- a/src/App.css
+++ b/src/App.css
@@ -1,6 +1,9 @@
-/* Styles by area, in cascade order. */
-@import "./styles/base.css";
-@import "./styles/mantine.css";
+/* Layers, lowest to highest. */
+@layer theme, base, mantine, vendor, overrides, components, utilities;
+@import "tailwindcss/theme.css" layer(theme);
+@import "./styles/base.css";
+@import "./styles/mantine.css";
 @import "./styles/shell.css";
 @import "./styles/sidebar.css";
 @import "./styles/session.css";
@@ -20,4 +23,5 @@
 @import "./styles/markdown.css";
 @import "./styles/chat.css";
-@import "./styles/chat-diff.css";
+@import "./styles/diff.css";
 @import "./styles/workbench.css";
`;
const patchB = `--- a/src/components/Boot.tsx
+++ b/src/components/Boot.tsx
@@ -12,8 +12,12 @@ export type Tone = "running" | "ok" | "bad";
 export function BootHero({ tone, progress, title }: Props) {
-  const offset = progress == null ? RING_C * 0.72 : RING_C * (1 - progress);
+  const offset = progress == null ? C * 0.72 : C * (1 - progress);
   return (
-    <div className={\`boot-hero \${tone}\`}>
+    <div className={\`grid justify-items-center \${TONE_TEXT[tone]}\`}>
       <h1 role="status">{title}</h1>
     </div>
   );
 }
`;
const turn1 = "u1";
export const items = [
  base(turn1, "user_message", "completed", { id: "u1", text: "Add Tailwind to the UI and give the diff view a proper open-source component. Keep Mantine for the controls." }),
  base(turn1, "reasoning", "completed", { text: "I'll look at how styles are loaded first, then wire Tailwind through Vite and replace the diff renderer." }),
  base(turn1, "tool", "completed", { action: "read", title: "Read", subject: "vite.config.ts", files: ["vite.config.ts"] }),
  base(turn1, "tool", "completed", { action: "read", title: "Read", subject: "src/App.css", files: ["src/App.css"] }),
  base(turn1, "tool", "completed", { action: "code-search", title: "Search", subject: "diff2html" }),
  base(turn1, "command_execution", "completed", { action: "command", title: "Ran", subject: "npm i -D tailwindcss @tailwindcss/vite", detail: "added 12 packages in 4s\n\nfound 0 vulnerabilities" }),
  base(turn1, "assistant_message", "completed", { text: "Tailwind is installed. Now the layer order, then the diff view." }),
  base(turn1, "file_change", "completed", { action: "edit", title: "Edited", subject: "src/App.css", files: ["src/App.css"], changes: [{ path: "src/App.css", origin: "provider", kind: "modified", additions: 6, deletions: 3, patch: patchA }] }),
  base(turn1, "file_change", "completed", { action: "edit", title: "Edited", subject: "src/components/Boot.tsx", files: ["src/components/Boot.tsx"], changes: [{ path: "src/components/Boot.tsx", origin: "provider", kind: "modified", additions: 2, deletions: 2, patch: patchB }, { path: "src/styles/diff.css", origin: "workspace", kind: "added", additions: 34, deletions: 0, patch: null }, { path: "src/styles/chat-diff.css", origin: "workspace", kind: "deleted", additions: 0, deletions: 161, patch: null }] }),
  base(turn1, "assistant_message", "completed", { text: `Done. Here is what changed:

## Summary

- **Tailwind 4** is wired through \`@tailwindcss/vite\`, and the cascade layers are declared in \`App.css\`.
- The diff view now uses \`@git-diff-view/react\` instead of injecting HTML strings.
- Old \`d2h\` styles are gone.

\`\`\`ts
export default defineConfig({
  plugins: [tailwindcss(), react()],
});
\`\`\`

| File | Change |
|---|---|
| \`App.css\` | layers |
| \`Boot.tsx\` | ring math |

Run \`npm run build\` to confirm.` }),
];
const turn2 = "u2";
const ask = [
  base("u2", "approval_request", "waiting", { requestId: "r1", title: "Run a command", text: "npm run build && npm test", detail: "cwd: /home/dev/cabin\nsandbox: workspace-write", approval: { kind: "command", decisions: [{ decision: "decline", label: "Decline", warning: null }, { decision: "acceptAlways", label: "Always allow", warning: null }, { decision: "accept", label: "Approve", warning: null }] } }),
];
export const items2 = [
  base(turn2, "user_message", "completed", { id: "u2", text: "Great. Now redesign the chat composer." }),
  base(turn2, "reasoning", "running", { text: "Reading the composer first." }),
  base(turn2, "tool", "completed", { action: "read", title: "Read", subject: "src/components/chat/ChatComposer.tsx", files: ["src/components/chat/ChatComposer.tsx"] }),
  base(turn2, "tool", "running", { action: "read", title: "Read", subject: "src/styles/chat-composer.css", files: ["src/styles/chat-composer.css"] }),
];
const ctl = { agentName: "Claude Code", agentVersion: "2.1.0", options: [{ id: "effort", name: "Reasoning effort", description: null, category: "thought_level", kind: "select", current: "medium", hidden: false, choices: [{ value: "low", name: "Low", description: "Fast", group: null }, { value: "medium", name: "Medium", description: null, group: null }, { value: "high", name: "High", description: "Thorough", group: null }] }], commands: [{ name: "compact", description: "Summarize the conversation", hint: null }], plan: [
  { content: "Wire Tailwind", priority: "high", status: "completed" }, { content: "Replace the diff view", priority: "high", status: "completed" }, { content: "Redesign the composer", priority: "medium", status: "in_progress" }, { content: "Verify in the app", priority: "low", status: "pending" }],
  context: { used: 42000, size: 200000, costMicros: 480000, currency: "USD" }, title: "Add Tailwind", suggestions: [], canSteer: true, images: true,
  capabilities: { prompt: { images: true, audio: false, embeddedContext: true }, steer: true, resume: true, load: true, fork: false, list: false, plans: true, subagents: true, asyncTasks: true, notices: true, compaction: true, fileRead: true, fileWrite: true, terminals: true, goal: false }, goal: null };
const mode = new URLSearchParams(location.search).get("m") ?? "idle";
const plan = { planId: "p1", agent: "claude", sessionId: "s1", revision: 2, title: "Redesign the chat composer", content: "## Plan\n\n1. Replace the composer shell with a glass dock\n2. Add an attach button and a morphing send/stop button\n3. Move the slash menu to Tailwind\n\nRisks: none.", contentHash: "x", complete: true, reviewKind: "native_request", nativePlanId: "n1", execPermission: "auto-accept-edits", status: "awaiting_review", execution: "not_started", decision: null, feedback: null, decisionIntentAt: null, appliedAt: null, createdAt: now, updatedAt: now, supersededBy: null };
export const snapshot = () => ({
  plansOut: mode === "plan" ? [plan] : [],
  items: mode === "plan" ? [...items, ...items2.slice(0, 1)] : mode === "ask" ? [...items, ...items2.slice(0, 3), ...ask] : mode === "run" ? [...items, ...items2] : mode === "empty" ? [] : items, plans: mode === "plan" ? [plan] : [], subagents: [], tasks: [],
  state: { state: mode === "run" || mode === "ask" ? "running" : "ready", message: null }, model: "claude-sonnet-5-5", permission: "approval-required", usage: [{ turnId: "u1", usage: { input: 12000, cached: 8000, cacheWrite: 0, output: 1800, reasoning: 300, generationMs: 9000, costMicros: 12000 } }], controls: ctl,
});
const gitFile = { path: "notes.txt", origPath: null, status: "M", additions: 1, deletions: 0 };
const gitStatus = { repository: true, branch: "main", head: "abcdef0", ahead: 0, behind: 0,
  staged: [] as typeof gitFile[], unstaged: [gitFile], untracked: [], conflicted: [], truncated: false };
const gitTest = {
  status: gitStatus,
  defer: false,
  release: null as (() => void) | null,
  commits: [] as boolean[],
  stage() { this.status = { ...gitStatus, staged: [gitFile], unstaged: [] }; },
};
if (mode === "git") (window as any).__gitTest = gitTest;

const terminalData = [
  "\x1b[36mnic@cabin\x1b[0m \x1b[34m~/projects/cabin\x1b[0m % git status --short",
  " \x1b[33mM\x1b[0m src/components/Terminal.tsx",
  " \x1b[33mM\x1b[0m src/styles/session.css",
  "",
  "\x1b[36mnic@cabin\x1b[0m \x1b[34m~/projects/cabin\x1b[0m % npm test",
  "\x1b[1;32m✓\x1b[0m Terminal metrics and resize",
  "\x1b[1;32m✓\x1b[0m Shell input and Unicode",
  "",
  "┌──────────────────────┐",
  "│  UTF-8 你好  日本語  │",
  "└──────────────────────┘",
  "",
  "\x1b[36mnic@cabin\x1b[0m \x1b[34m~/projects/cabin\x1b[0m % ",
].join("\r\n");
const terminalTest = {
  status: { state: "running", pid: 1 } as import("../../src/services/ipc/generated/core").SessionStatus,
  sizes: [] as { cols: number; rows: number }[],
  writes: [] as string[],
  snapshot() {
    return { generation: 1, status: this.status, startOffset: 0,
      endOffset: new TextEncoder().encode(terminalData).length, data: terminalData };
  },
};
if (mode === "terminal") (window as any).__terminalTest = terminalTest;

(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 1, unregisterCallback: () => {},
  invoke: async (cmd: string, args: any) => {
    if (cmd === "guest_invoke") {
      if (mode === "git" && args.method === "git_status") {
        const status = gitTest.status;
        if (gitTest.defer) {
          gitTest.defer = false;
          await new Promise<void>((resolve) => { gitTest.release = resolve; });
        }
        return status;
      }
      if (mode === "git" && args.method === "git_commit") {
        gitTest.commits.push(args.params.all);
        gitTest.status = { ...gitStatus, head: "abcdef1", unstaged: [] };
        return { head: "abcdef1", summary: "Update notes" };
      }
      if (mode === "terminal") {
        if (args.method === "terminal_snapshot") return terminalTest.snapshot();
        if (args.method === "terminal_attach") {
          terminalTest.status = { state: "running", pid: 1 };
          return terminalTest.snapshot();
        }
        if (args.method === "session_resize") terminalTest.sizes.push(args.params);
        if (args.method === "session_write") terminalTest.writes.push(args.params.data);
      }
      if (args.method === "chat_snapshot") return snapshot();
      if (args.method === "agent_chat_capabilities") return [{ agent: "claude", permissionModes: ["approval-required", "auto-accept-edits", "auto", "plan", "full-access"] }];
      if (args.method === "agent_models") return [
        { id: "claude-opus-5-5", name: "Opus 5.5", provider: "Anthropic", description: "Most capable, for complex work", isDefault: true },
        { id: "claude-sonnet-5-5", name: "Sonnet 5.5", provider: "Anthropic", description: "Fast and balanced", isDefault: false },
        { id: "claude-haiku-5-5", name: "Haiku 5.5", provider: "Anthropic", description: "Quickest for small tasks", isDefault: false },
        { id: "gpt-5", name: "GPT-5", provider: "OpenAI", description: null, isDefault: false }];
      if (args.method === "agent_catalog") return { commands: [{ name: "review", description: "Review the pending changes", hint: null, kind: "command", source: "claude", insert: "/review " }, { name: "pdf", description: "Read and fill PDFs", hint: "<file>", kind: "skill", source: "~/.claude/skills", insert: "/pdf " }, { name: "compact", description: "Summarize the conversation", hint: null, kind: "command", source: "agent", insert: "/compact " }], mcp: [], skills: [] };
      return null;
    }
    return null;
  },
};
