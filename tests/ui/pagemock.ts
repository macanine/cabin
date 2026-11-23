import type { NetworkRoute, NetworkStatus } from "../../src/services/ipc/generated/core";
import { initialState } from "../../src/state/reducer";

// Plausible guest/host replies for page work. Anything unknown answers null.
export function mockIpc(networkMode?: string | null) {
  const now = Date.now();
  const kinds = ["claude", "codex", "opencode", "pi", "antigravity"];
  const folder = "/Users/dev/Documents/site";
  let allowed = false;
  let usageImported = false;
  let rejectApply = networkMode === "reject";
  const running = networkMode !== "stopped";
  const paused = networkMode === "paused";
  const network: NetworkStatus = {
    ...initialState().network, available: true, running, paused, revision: "r", gatewaySession: running ? 1 : null,
    gate: running && !paused ? "open" : "closed", expiresAt: running && !paused ? now + 3600e3 : null,
  };
  const access = () => allowed ? { state: "available", message: null, privacySettings: false }
    : { state: "readDenied", message: "Cabin needs permission to read this folder.", privacySettings: true };
  const handlers: Record<string, (p: any) => unknown> = {
    mac_open_file_settings: () => { allowed = true; },
    mac_permissions: () => [{ id: "sync-site", name: "Site", path: folder, needsWrite: true, access: access() }],
    mac_folder_access: ({ path, request }) => {
      if (path !== folder) throw new Error("Unexpected folder access request");
      if (request) allowed = true;
      return access();
    },
    "plugin:dialog|open": () => folder,
    sync_preview: ({ hostPath }) => {
      if (hostPath !== folder) throw new Error("Unexpected folder preview");
      if (!allowed) throw { code: "mac_folder_privacy", message: access().message };
      return { hostPath, items: [], files: 0, bytes: 0, git: false, gitignore: false };
    },
    agent_chat_capabilities: () => [],
    acp_runtimes: () => [],
    network_state: () => ({ ...network }),
    network_exit: () => ({
      ip: "222.205.120.142", countryCode: "CN", country: "China", region: "Shanghai", city: "Shanghai",
      isp: "China Education and Research Network", ipType: null, asn: 4538,
      latitude: 31.23, longitude: 121.47, timezone: "Asia/Shanghai", source: "IP.SB", checkedAt: now,
    }),
    network_exit_type: () => ({ ipType: "commercial", inferred: false, organization: "China Education and Research Network", asn: 4538, source: "IPLocate" }),
    network_origin: () => ({
      ip: "10.196.103.142", countryCode: "CN", country: "China", region: "Zhejiang", city: "Hangzhou",
      isp: null, ipType: null, asn: null, latitude: 30.27, longitude: 120.15, timezone: "Asia/Shanghai", source: "IP.SB", checkedAt: now,
    }),
    network_system_route: () => ({ internetInterface: "utun1024", defaultInterface: "en0", tunnel: true, fakeIp: true }),
    environment_review: () => ({
      fields: [
        { field: "timezone", enabled: true, reason: null, suggestion: null },
        { field: "language", enabled: true, reason: null, suggestion: null },
      ],
      changes: [], blocked: null,
      timezones: ["America/New_York", "Asia/Shanghai", "Asia/Tokyo", "UTC"],
      languages: ["C.UTF-8", "en_US.UTF-8", "zh_CN.UTF-8"],
    }),
    network_interfaces: () => [
      { name: "en0", label: "Wi-Fi (en0)", kind: "wifi", available: true, ipv4: ["192.0.2.2"] },
      { name: "en8", label: "USB adapter (en8)", kind: "hardware", available: true, ipv4: ["192.0.2.3"] },
      { name: "en9", label: "Ethernet (en9)", kind: "ethernet", available: false, ipv4: [] },
    ],
    network_apply: ({ route }: { route: NetworkRoute }) => {
      if (route.enabled || route.chain.length || ![null, "en8"].includes(route.bindInterface)) throw new Error("Unexpected network route");
      if (rejectApply) {
        rejectApply = false;
        throw { code: "network_check", message: "The interface did not pass verification" };
      }
      network.route = route;
      network.generation++;
      return { ...network };
    },
    claude_usage: ({ accountId }) => {
      if (accountId !== "claude-team" && accountId !== "claude-personal") throw new Error("Unexpected Claude account ID");
      const usedPercent = accountId === "claude-team" ? 37 : 81;
      return { windows: [{ label: "5 hours", usedPercent, resetsAt: now + 3600e3, resetText: null }], noLimits: false, refreshedAt: now };
    },
    codex_accounts: () => ({ accounts: [], activeAccountId: null, quota: null }),
    node_state: () => ({ active: null, minimumMajor: 20, recommendedMajor: 24, installed: [] }),
    node_releases: ({ mirror }) => [{
      version: mirror === "https://nodejs.org/dist" ? "v24.9.0" : "v24.10.0", major: 24,
      date: "2026-01-13", lts: "Krypton", npm: null, security: false, installed: false,
    }],
    setup_checks: () => [],
    session_create: ({ workspaceId, agent, prompt }) => {
      if (workspaceId !== "w1" || agent !== "antigravity" || prompt !== null) throw new Error("Unexpected sign-in terminal request");
      return { id: "agy-login" };
    },
    mcp_library: () => {
      const b = (on: (boolean | undefined)[]) => kinds.map((kind, i) => ({ kind, state: on[i] === undefined ? "unsupported" : on[i] ? "enabled" : "disabled", wanted: !!on[i] }));
      return { agents: kinds.map((kind) => ({ kind, path: "/home/dev/." + kind + "/mcp.json", error: null, supportsSse: kind === "claude" || kind === "opencode" })), rows: [
        { name: "filesystem", endpoint: "npx -y @modelcontextprotocol/server-filesystem /home/dev", transport: "stdio", description: "Read and write files in the workspace", managed: true, conflict: false, bindings: b([true, true, true, false, false]) },
        { name: "github", endpoint: "https://api.githubcopilot.com/mcp/", transport: "HTTP", description: "Issues, pull requests and code search", managed: true, conflict: false, bindings: b([true, false, true, false, false]) },
        { name: "events", endpoint: "https://example.com/sse", transport: "SSE", description: "Event stream", managed: true, conflict: false, bindings: b([true, undefined, false, undefined, undefined]) },
        { name: "playwright", endpoint: "npx @playwright/mcp@latest", transport: "stdio", description: "", managed: true, conflict: false, bindings: b([true, true, false, false, false]) },
        { name: "postgres", endpoint: "postgresql://localhost/dev", transport: "stdio", description: "", managed: false, conflict: false, bindings: b([false, false, true, false, false]) },
      ] };
    },
    skill_library: () => {
      const b = (on: boolean[]) => kinds.map((kind, i) => ({ kind, state: on[i] ? "enabled" : "disabled", wanted: !!on[i] }));
      const sk = (name: string, description: string, on: boolean[], repo: string | null) => ({ skill: { folder: name, name, description, mode: "link", origin: repo ? { repository: { repository: repo, branch: "main", directory: "skills" }, path: name, revision: "abc" } : null }, bindings: b(on) });
      return { installed: [sk("pdf", "Read, fill and create PDF files", [true, true, true, false], "anthropics/skills"), sk("frontend-design", "Distinctive, production-grade interfaces", [true, false, true, false], "anthropics/skills"), sk("my-notes", "Personal notes helper", [true, false, false, false], null)], local: [], backups: [], repositories: [] };
    },
    usage_sync: () => { usageImported = true; return { imported: 12, errors: [] }; },
    usage_dashboard: () => {
      const day = 86400e3;
      const daily = Array.from({ length: 30 }, (_, i) => { const r = 4 + Math.round(18 * Math.abs(Math.sin(i / 3)) + (i % 5)); return { timestamp: now - (29 - i) * day, requests: r, input: r * 4200, output: r * 900, cacheRead: r * 9000, cacheWrite: r * 300, cost: r * 0.043, unpricedRequests: 0 }; });
      const sum = (k: string) => daily.reduce((a: number, d: any) => a + d[k], 0);
      const agent = (a: string, f: number) => ({ agent: a, requests: Math.round(sum("requests") * f), input: Math.round(sum("input") * f), output: Math.round(sum("output") * f), cacheRead: Math.round(sum("cacheRead") * f), cacheWrite: 0, cost: sum("cost") * f, unpricedRequests: 0 });
      return {
        summary: { requests: sum("requests"), input: sum("input"), output: sum("output"), cacheRead: sum("cacheRead"), cacheWrite: sum("cacheWrite"), cost: usageImported ? sum("cost") : 0, unpricedRequests: 3 },
        daily, total: 412,
        models: [{ model: "claude-sonnet-5-5", requests: 240, input: 900000, output: 120000, cacheRead: 3000000, cacheWrite: 0, cost: 31.2, unpricedRequests: 0 }, { model: "gpt-5", requests: 120, input: 500000, output: 90000, cacheRead: 800000, cacheWrite: 0, cost: 14.8, unpricedRequests: 0 }, { model: "claude-haiku-5-5", requests: 52, input: 100000, output: 20000, cacheRead: 200000, cacheWrite: 0, cost: 1.1, unpricedRequests: 0 }],
        providers: [], agents: [agent("claude", 0.6), agent("codex", 0.3), agent("opencode", 0.1)],
        logs: Array.from({ length: 8 }, (_, i) => ({ id: "l" + i, agent: i % 3 ? "claude" : "codex", sessionId: null, model: i % 3 ? "claude-sonnet-5-5" : "gpt-5", provider: null, timestamp: now - i * 3600e3, input: 12000 + i * 700, output: 1800 + i * 90, cacheRead: 9000, cacheWrite: 0, cost: 0.04 + i * 0.01, durationMs: 4000, status: "ok" })),
        prices: [], priceSources: [],
      };
    },
  };
  (window as any).__TAURI_INTERNALS__ = {
    transformCallback: () => 1, unregisterCallback: () => {},
    invoke: async (cmd: string, a: any) => {
      if (cmd === "guest_invoke") { const h = handlers[a.method]; return h ? h(a.params) : null; }
      const h = handlers[cmd]; return h ? h(a) : null;
    },
  };
  return { handlers, now, network: { ...network } };
}
