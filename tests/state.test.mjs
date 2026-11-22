import assert from "node:assert/strict";
import test from "node:test";
import { activeOperations, applyEvent, fromSnapshot, initialState, MONITOR_HISTORY, upsertOperation } from "../src/state/reducer.ts";

const workspace = (id, name, extra = {}) => ({
  id, name, path: `/home/u/${name}`, proxy: { mode: "inherit" }, modelOverrides: [], archived: false,
  createdAt: 1, updatedAt: 1, ...extra,
});
const session = (id, workspaceId, name, extra = {}) => ({
  id, workspaceId, name, agent: "claude", archived: false, starred: false, createdAt: 1, updatedAt: 1, lastStartedAt: null,
  status: { state: "stopped" }, ...extra,
});
const op = (id, createdAt, state = "running") => ({
  id, kind: "t", title: id, state, steps: [], error: null, retryable: false, createdAt, updatedAt: createdAt,
});

function snapshot() {
  return {
    workspaces: [workspace("w1", "api"), workspace("w2", "web", { archived: true })],
  sessions: [session("s1", "w1", "Claude Code 1"), session("s2", "w1", "Pi bugfix", { archived: true })],
    agents: [], environment: null, proxy: { sources: [], nodes: [] },
    proxyRuntime: { running: false, port: null, nodeId: null, error: null },
    operations: [], settings: null, dataDir: "/data", hostname: "ubuntu",
  };
}

test("the initial network route uses the system interface", () => {
  assert.equal(initialState().network.route.bindInterface, null);
});

test("snapshot marks the state ready", () => {
  const state = fromSnapshot(initialState(), snapshot());
  assert.equal(state.ready, true);
  assert.equal(state.workspaces.length, 2);
});

test("session status events update only that session", () => {
  let state = fromSnapshot(initialState(), snapshot());
  state = applyEvent(state, { type: "sessionStatus", id: "s1", status: { state: "running", pid: 42 } });
  assert.deepEqual(state.sessions[0].status, { state: "running", pid: 42 });
  assert.deepEqual(state.sessions[1].status, { state: "stopped" });
});

test("session output never changes state", () => {
  const state = fromSnapshot(initialState(), snapshot());
  assert.equal(applyEvent(state, { type: "sessionOutput", id: "s1", data: "hi" }), state);
});

test("chat activity moves a session's last-active time", () => {
  let state = fromSnapshot(initialState(), snapshot());
  state = applyEvent(state, { type: "chatItem", id: "s1", item: { updatedAt: 100_000 } });
  assert.equal(state.sessions[0].updatedAt, 100_000);
  assert.equal(
    applyEvent(state, { type: "chatItem", id: "s1", item: { updatedAt: 110_000 } }),
    state,
    "updates inside the throttle window are ignored",
  );
});

test("operations are upserted newest first", () => {
  let list = upsertOperation([], op("a", 1));
  list = upsertOperation(list, op("b", 2));
  list = upsertOperation(list, op("a", 1, "succeeded"));
  assert.deepEqual(list.map((o) => o.id), ["b", "a"]);
  assert.equal(list[1].state, "succeeded");
  assert.deepEqual(activeOperations(list).map((o) => o.id), ["b"]);
});

test("monitor history is bounded", () => {
  let state = initialState();
  for (let i = 0; i < MONITOR_HISTORY + 5; i += 1) {
    state = applyEvent(state, { type: "monitor", sample: { at: i, cpuPercent: 1, memoryUsed: null, memoryTotal: null, latencyMs: null, downloadBps: null, uploadBps: null } });
  }
  assert.equal(state.monitor.length, MONITOR_HISTORY);
  assert.equal(state.monitor[0].at, 5);
});

test("machine events gate the guest and a snapshot restores it", async () => {
  const { fromHostSnapshot } = await import("../src/state/reducer.ts");
  const machine = (connection) => ({
    state: "running", connection, busy: null, sshPort: 2222, os: "ubuntu-24.04", cpus: 4, memoryMb: 4096,
    diskSize: "20G", display: false, browser: false, device: false, guestVersion: null, error: null,
  });
  let state = fromHostSnapshot(initialState(), { machine: machine("connecting"), guest: null });
  assert.equal(state.ready, true);
  assert.equal(state.guestReady, false);
  state = applyEvent(state, { type: "snapshot", snapshot: snapshot() });
  assert.equal(state.guestReady, true);
  state = applyEvent(state, { type: "machine", status: machine("offline") });
  assert.equal(state.guestReady, false, "losing the link hides guest views");
  assert.equal(state.workspaces.length, 2, "but keeps the last data for when it comes back");
});

test("destroying the machine clears guest data and ignores stale snapshots", () => {
  const running = { state: "running", connection: "connected", busy: null };
  let state = { ...fromSnapshot(initialState(), snapshot()), machine: running, install: [{ id: "old" }] };
  state = applyEvent(state, { type: "machine", status: { ...running, state: "notInstalled", connection: "offline" } });
  assert.deepEqual(state.workspaces, []);
  assert.deepEqual(state.sessions, []);
  assert.deepEqual(state.install, []);
  assert.equal(state.guestReady, false);
  assert.equal(applyEvent(state, { type: "snapshot", snapshot: snapshot() }), state);
});
