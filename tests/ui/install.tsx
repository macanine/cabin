// First-run journey harness: the real <App/> against a scripted host that is
// not installed yet. Query: ?host=mac|win|win-nowhpx|win-noqemu&fail=image|provision&theme=dark
import ReactDOM from "react-dom/client";
import App from "../../src/App";
import { initialState } from "../../src/state/reducer";

const q = new URLSearchParams(location.search);
if (q.get("theme")) document.documentElement.dataset.theme = q.get("theme")!;
const host = q.get("host") ?? "mac";
const fail = q.get("fail");
const windows = host.startsWith("win");
let whpxOn = host !== "win-nowhpx";

const callbacks = new Map<number, (raw: unknown) => void>();
let nextCallback = 1;
const indexes = new Map<number, number>();
function send(channel: { id: number }, message: unknown) {
  const index = indexes.get(channel.id) ?? 0;
  indexes.set(channel.id, index + 1);
  callbacks.get(channel.id)?.({ index, message });
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

let events: { id: number } | null = null;
const emit = (event: unknown) => events && send(events, event);
let machine: any = {
  state: "notInstalled", connection: "offline", busy: null, sshPort: null, cpus: null, memoryMb: null, diskSize: null,
  display: false, os: null, browser: false, device: false, guestVersion: null, username: null, hostname: null, error: null,
};
const setMachine = (patch: any) => { machine = { ...machine, ...patch }; emit({ type: "machine", status: machine }); };

const PIPELINE: [string, string][] = [
  ["qemu", "Verify the bundled QEMU and UEFI firmware"], ["image", "Download and verify the OS cloud image"],
  ["disk", "Create resized qcow2 disk + UEFI variable store"], ["seed", "Generate cloud-init seed"],
  ["provision", "Boot, run cloud-init, verify user/hostname/network/proxy"],
  ["desktop", "Install the desktop stack"], ["browser", "Install the CDP browser fingerprint profile"],
];
const step = (id: string, title: string, state = "pending", extra: any = {}) => ({
  id, title, state, detail: null, logTail: [], logPath: null, seconds: null, appliedAt: null, fingerprint: null, download: null, ...extra,
});

function readiness() {
  const items: any[] = [];
  if (windows) {
    items.push(whpxOn
      ? { id: "accel", label: "Hardware acceleration", ok: true, optional: true, detail: "Windows Hypervisor Platform", fix: null }
      : { id: "accel", label: "Hardware acceleration", ok: false, optional: true, detail: "Windows Hypervisor Platform is turned off. The machine will run emulated, which is roughly 5–10× slower.", fix: { kind: "enableWhpx" } });
  } else {
    items.push({ id: "accel", label: "Hardware acceleration", ok: true, optional: true, detail: "Apple Hypervisor", fix: null });
  }
  items.push(host === "win-noqemu"
    ? { id: "qemu", label: "Virtual machine engine", ok: false, optional: false, detail: "Cabin's bundled QEMU is missing", fix: { kind: "reinstallApp" } }
    : { id: "qemu", label: "Virtual machine engine", ok: true, optional: false, detail: "QEMU 11.1", fix: null });
  items.push({ id: "ssh", label: "SSH", ok: true, optional: false, detail: "Bundled", fix: null });
  items.push({ id: "agentd", label: "Cabin VM service", ok: true, optional: false, detail: "", fix: null });
  items.push({ id: "disk", label: "Free disk space", ok: true, optional: false, detail: "212 GB free", fix: null });
  items.push({ id: "memory", label: "Memory", ok: true, optional: true, detail: "16 GB", fix: null });
  return { items, host: { memoryMb: 16384, cpus: 10, maxDiskGb: 209, username: "Nic Chen", deviceProfileSupported: !windows, platform: windows ? "windows" : "macos" } };
}

const guest = () => ({
  workspaces: [], sessions: [], agents: [], environment: initialState().environment, proxy: initialState().proxy,
  network: initialState().network, operations: [], settings: { defaultAgent: "claude", latencyTarget: "1.1.1.1:443", checkTarget: "example.com:443" },
  dataDir: "/home/nic/.local/share/cabin", hostname: "cabin",
});

async function install(channel: { id: number }) {
  setMachine({ busy: "Installing the machine" });
  const steps = PIPELINE.map(([id, title]) => step(id, title));
  const show = () => emit({ type: "install", steps: steps.map((s) => ({ ...s })) });
  for (const s of steps) {
    s.state = "running"; show();
    if (s.id === "image") {
      const total = 612e6;
      for (let done = 0; done <= total; done += total / 6) {
        s.download = { doneBytes: Math.min(done, total), totalBytes: total, bytesPerSec: 24e6, verifying: false } as any; show(); await sleep(400);
        if (fail === "image" && done > total / 2) {
          s.state = "failed"; s.detail = "could not download https://cloud-images.ubuntu.com/minimal/releases/noble/release-20261101/ubuntu-24.04-minimal-cloudimg-amd64.img: connection reset by peer";
          s.logTail = ["GET … 206", "connection reset by peer"]; show();
          setMachine({ busy: null });
          send(channel, { type: "failed", error: { code: "install", message: s.detail } });
          return;
        }
      }
    }
    await sleep(s.id === "provision" ? 1200 : 350);
    if (fail === "provision" && s.id === "provision") {
      s.state = "failed"; s.detail = "timed out after 900s waiting for cloud-init"; s.logTail = ["cloud-init status: running"]; show();
      setMachine({ busy: null });
      send(channel, { type: "failed", error: { code: "install", message: "Boot, run cloud-init: timed out after 900s waiting for cloud-init" } });
      return;
    }
    s.state = "done"; s.seconds = 3; show();
  }
  setMachine({ state: "running", connection: "connecting", busy: null, os: "ubuntu-24.04", username: "nic", hostname: "cabin", cpus: 4, memoryMb: 4096, diskSize: "20G" });
  await sleep(800);
  setMachine({ connection: "connected", guestVersion: "0.2.0" });
  emit({ type: "snapshot", snapshot: guest() });
  send(channel, { type: "completed", result: machine });
}

const handlers: Record<string, (a: any) => unknown> = {
  app_subscribe: ({ channel }) => { events = channel; return { machine, guest: null, start: null }; },
  machine_checks: () => readiness(),
  network_state: () => ({ ...initialState().network, available: true, running: false }),
  proxy_catalog: () => initialState().proxy,
  usage_dashboard: () => {
    const zero = { requests: 0, input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0, unpricedRequests: 0 };
    return { summary: zero, daily: [], total: 0, models: [], providers: [], agents: [], logs: [], prices: [], priceSources: [] };
  },
  host_fix: async ({ fix }) => {
    await sleep(600);
    if (fix.kind === "enableWhpx") return { restartRequired: true };
    return { restartRequired: false };
  },
  machine_plan: ({ options }) => ({
    os: options.os ?? "ubuntu-24.04", osName: "Ubuntu 24.04 LTS",
    osChoices: [
      { id: "ubuntu-24.04", name: "Ubuntu 24.04 LTS", testing: false, mirrors: ["http://archive.ubuntu.com/ubuntu", "https://mirrors.ustc.edu.cn/ubuntu"] },
      { id: "ubuntu-26.04", name: "Ubuntu 26.04 LTS", testing: false, mirrors: ["http://archive.ubuntu.com/ubuntu"] },
    ],
    imageIndex: "https://cloud-images.ubuntu.com/minimal/releases/noble/", qemuRelease: "11.1.0", qemuInstalled: true,
    cpus: options.cpus ?? 4, memoryMb: options.memoryMb ?? 4096, diskSize: options.diskSize ?? "20G", network: "gateway",
    accel: windows ? (whpxOn ? "whpx" : "tcg") : "hvf", firmware: null, diskPath: "C:\\Users\\nic\\AppData\\Roaming\\cabin\\disk.qcow2",
    seedPath: "", username: options.username ?? "nic", hostname: options.hostname ?? "cabin", sshPort: 2222, aptMirror: null,
    steps: PIPELINE.map(([id, title]) => step(id, title)),
  }),
  machine_test_mirrors: () => [
    { uri: "http://archive.ubuntu.com/ubuntu", host: "archive.ubuntu.com", default: true, status: "ok", latencyMs: 210, speedKbps: 3000, detail: "" },
    { uri: "https://mirrors.ustc.edu.cn/ubuntu", host: "mirrors.ustc.edu.cn", default: false, status: "ok", latencyMs: 35, speedKbps: 40000, detail: "" },
  ],
  machine_install: ({ channel }) => { void install(channel); return null; },
  machine_resume: ({ channel }) => { void install(channel); return null; },
  machine_cancel_install: () => null,
  machine_verify: () => [],
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
(window as any).__journey = { enableWhpx: () => { whpxOn = true; } };
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: (fn: (raw: unknown) => void) => { const id = nextCallback++; callbacks.set(id, fn); return id; },
  unregisterCallback: (id: number) => callbacks.delete(id),
  invoke: async (cmd: string, a: any) => {
    if (cmd === "guest_invoke") { const g = handlers[a.method]; return g ? g(a.params) : null; }
    const h = handlers[cmd];
    if (h) return h(a);
    if (/(_list|syncs|_interfaces|_permissions)$/.test(cmd)) return [];
    return null;
  },
  metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
};
ReactDOM.createRoot(document.getElementById("root")!).render(<App />);
