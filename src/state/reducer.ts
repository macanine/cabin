// Pure state transitions for the app snapshot + event stream.
// No React or Tauri imports here so `node --test` can exercise it directly.
import type { NetworkRoute } from "../lib/networkRoute";
import type {
  AgentStatus, AppEvent, AppSettings, AppSnapshot, EnvironmentState, HostSnapshot, InstallStep, MachineStatus,
  MonitorSample,
  OperationRecord, StartReport, ProxyCatalog, NetworkStatus, Session, SyncView, Workspace,
} from "../services/ipc/generated/core";

export const MONITOR_HISTORY = 60;
export const OPERATION_LIMIT = 50;

export interface AppState {
  /** The Mac app answered (machine state is known). */
  ready: boolean;
  /** cabind in the VM is connected and its snapshot is loaded. */
  guestReady: boolean;
  machine: MachineStatus | null;
  /** Pipeline steps of the running (or last) machine install. */
  install: InstallStep[];
  /** The running or most recent machine start. */
  start: StartReport | null;
  workspaces: Workspace[];
  sessions: Session[];
  agents: AgentStatus[];
  environment: EnvironmentState | null;
  proxy: ProxyCatalog;
  /** The VM's route and gateway (owned by the Mac app). */
  network: NetworkStatus;
  networkLoaded: boolean;
  networkLoadError: string | null;
  operations: OperationRecord[];
  settings: AppSettings | null;
  monitor: MonitorSample[];
  dataDir: string;
  hostname: string;
  /** Mac folders synced with folders in the machine (the Mac app owns them). */
  syncs: SyncView[];
}

export function initialState(): AppState {
  return {
    ready: false,
    guestReady: false,
    machine: null,
    install: [],
    start: null,
    workspaces: [],
    sessions: [],
    agents: [],
    environment: null,
    proxy: { sources: [], nodes: [], groups: [] },
    network: {
      available: false,
      route: { enabled: false, chain: [], bindInterface: null } as NetworkRoute,
      running: false,
      error: null,
      gateError: null,
      gate: "closed",
      paused: false,
      autoRecover: true,
      regionFollowsExit: true,
      revision: "",
      generation: 0,
      checkedAt: null,
      expiresAt: null,
      nextRetryAt: null,
      retryAttempt: 0,
      report: null,
      connectivity: "normal",
      failedChecks: 0,
      connectivityError: null,
      gatewaySession: null,
    },
    networkLoaded: false,
    networkLoadError: null,
    operations: [],
    settings: null,
    monitor: [],
    dataDir: "",
    hostname: "",
    syncs: [],
  };
}

export function fromSnapshot(state: AppState, snapshot: AppSnapshot): AppState {
  return {
    ...state,
    ready: true,
    guestReady: true,
    workspaces: snapshot.workspaces,
    sessions: snapshot.sessions,
    agents: snapshot.agents,
    environment: snapshot.environment,
    proxy: snapshot.proxy,
    network: snapshot.network,
    networkLoaded: true,
    networkLoadError: null,
    operations: snapshot.operations,
    settings: snapshot.settings,
    dataDir: snapshot.dataDir,
    hostname: snapshot.hostname,
  };
}

export function fromHostSnapshot(state: AppState, host: HostSnapshot): AppState {
  const next = { ...state, ready: true, machine: host.machine, start: host.start ?? state.start };
  return host.guest && host.machine.state !== "notInstalled"
    ? fromSnapshot(next, host.guest)
    : { ...next, guestReady: false };
}

export function upsertOperation(list: OperationRecord[], record: OperationRecord): OperationRecord[] {
  const next = list.filter((item) => item.id !== record.id);
  next.push(record);
  next.sort((a, b) => b.createdAt - a.createdAt);
  return next.slice(0, OPERATION_LIMIT);
}

/** Apply one backend event. `sessionOutput` is routed to terminals, not state. */
export function applyEvent(state: AppState, event: AppEvent): AppState {
  switch (event.type) {
    case "operation":
      return { ...state, operations: upsertOperation(state.operations, event.record) };
    case "terminalStatus":
    case "sessionStatus":
      return {
        ...state,
        sessions: state.sessions.map((session) =>
          session.id === event.id ? { ...session, status: event.status } : session),
      };
    case "monitor":
      return { ...state, monitor: [...state.monitor, event.sample].slice(-MONITOR_HISTORY) };
    case "agents":
      return { ...state, agents: event.agents };
    case "network":
      return { ...state, proxy: event.catalog, network: event.status, networkLoaded: true, networkLoadError: null };
    case "environment":
      return { ...state, environment: event.state };
    case "library":
      // Library events carry the supervisor's current session statuses.
      return { ...state, workspaces: event.workspaces, sessions: event.sessions };
    case "machine":
      if (event.status.state === "notInstalled" && state.machine?.state !== "notInstalled") {
        return { ...initialState(), ready: state.ready, machine: event.status, syncs: state.syncs };
      }
      return {
        ...state,
        machine: event.status,
        guestReady: event.status.connection === "connected" ? state.guestReady : false,
        monitor: event.status.connection === "connected" ? state.monitor : [],
      };
    case "snapshot":
      if (state.machine?.state === "notInstalled") return state;
      return fromSnapshot(state, event.snapshot);
    case "install":
      return { ...state, install: event.steps };
    case "start":
      return { ...state, start: event.report };
    case "sync":
      return {
        ...state,
        syncs: state.syncs.map((view) => (view.project.id === event.status.id ? { ...view, status: event.status } : view)),
      };
    case "sessionOutput":
      return state;
    case "chatItem": {
      // Keep the task's "last active" figure current while a chat runs. The
      // store's session row is not rewritten for every token; update it in
      // place, at most once per 15 seconds, so the sidebar age moves.
      const index = state.sessions.findIndex((session) => session.id === event.id);
      const at = event.item.updatedAt;
      if (index < 0 || at < state.sessions[index].updatedAt + 15_000) return state;
      const sessions = [...state.sessions];
      sessions[index] = { ...sessions[index], updatedAt: at };
      return { ...state, sessions };
    }
    case "chatPlan":
    case "chatSubagent":
    case "chatTask":
    case "chatState":
    case "chatModel":
    case "chatPermission":
    case "chatUsage":
    case "chatControls":
      return state;
  }
}

export function activeOperations(operations: OperationRecord[]): OperationRecord[] {
  return operations.filter((op) => op.state === "pending" || op.state === "running");
}
