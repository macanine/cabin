/**
 * Disconnect-resilient terminal session controller and LRU pool.
 *
 * Keeps terminal emulators (and their full scrollback + scroll position) alive
 * across session switches and transient guest disconnects at 0% idle CPU.
 * Reconciles snapshots, live output, PTY resizing, and automatic shell recovery.
 */
import type { Ghostty } from "ghostty-web";
import type { SessionStatus } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { libraryIpc } from "../../services/ipc/library";
import { machineIpc } from "../../services/ipc/app";
import { clipboard } from "../../services/ipc/dialogs";
import { appStore, onTerminalEvent } from "../../state/store";
import { uiActions } from "../../state/ui";
import { formatBytes } from "../format";
import {
  blobBase64, PASTE_IMAGE_MAX, settledSender, TerminalReplay, typedPath,
  type Grid, type TerminalStreamEvent,
} from "./protocol";
import {
  loadGhostty, TerminalEngine, type ContextMenuRequest, type ScrollState, type SearchState,
} from "./engine";
import {
  type ColorScheme, getTerminalFontSize, loadTerminalFonts, stepTerminalFontSize,
  subscribeTerminalFontSize,
} from "./theme";

export type ConnectionPhase = "connecting" | "connected" | "reconnecting" | "offline";

export interface TerminalViewState {
  phase: ConnectionPhase;
  status?: SessionStatus;
  busy: boolean;
  error?: string;
  grid: Grid;
  scroll: ScrollState;
}

export interface AttachCallbacks {
  onState: (state: TerminalViewState) => void;
  onSize?: (grid: Grid) => void;
  onSearchOpen: () => void;
  onSearchUpdate: (state: SearchState) => void;
  onContextMenu: (request: ContextMenuRequest) => void;
}

const POOL_LIMIT = 8;
const PENDING_LIMIT = 1024;
const PENDING_BYTES_LIMIT = 512 * 1024;
const INPUT_QUEUE_LIMIT = 64 * 1024;

type SyncMode = "entry" | "reconnect" | "recover" | "retry" | "stream";

export class TerminalSessionController {
  readonly sessionId: string;
  readonly engine: TerminalEngine;

  attached = false;
  autoStart = false;

  private replay = new TerminalReplay();
  private connected = appStore.get().guestReady;
  private phase: ConnectionPhase = appStore.get().guestReady ? "connecting" : "offline";
  private connectionEpoch = 0;
  private disposed = false;
  private busy = false;
  private activeMode: SyncMode | undefined;
  private queuedMode: SyncMode | undefined;
  private error: string | undefined;

  private grid: Grid;
  private gridMeasured = false;
  private gridReadyResolvers: (() => void)[] = [];
  private sentSize = "";
  private resizing = false;
  private readonly ptyResize: ReturnType<typeof settledSender>;

  private inputQueue = "";
  private inputInFlight = false;
  private writeGeneration: number | undefined;

  private pending: Extract<TerminalStreamEvent, { type: "sessionOutput" | "terminalStatus" }>[] = [];
  private pendingBytes = 0;

  private callbacks: AttachCallbacks | null = null;
  private published: TerminalViewState | undefined;
  private readonly unsubscribeEvents: () => void;
  private readonly unsubscribeFont: () => void;

  constructor(ghostty: Ghostty, sessionId: string, scheme: ColorScheme) {
    this.sessionId = sessionId;
    this.engine = new TerminalEngine(ghostty, {
      scheme,
      fontSize: getTerminalFontSize(),
    });
    this.grid = this.engine.getGrid();
    this.ptyResize = settledSender((grid) => {
      this.callbacks?.onSize?.(grid);
      this.pushResize(grid);
    });

    this.engine.setCallbacks({
      onData: (data) => this.sendInput(data),
      onResize: (grid) => {
        this.grid = grid;
        if (!this.gridMeasured) {
          this.gridMeasured = true;
          for (const resolve of this.gridReadyResolvers.splice(0)) resolve();
        }
        this.ptyResize.push(grid);
        this.publish();
      },
      onScroll: (scroll) => this.publish(scroll),
      onZoom: (delta) => { stepTerminalFontSize(delta); },
      onSearchOpen: () => this.callbacks?.onSearchOpen(),
      onSearchUpdate: (state) => this.callbacks?.onSearchUpdate(state),
      onCopy: (text) => { void copyToClipboard(text); },
      onPasteRequest: () => readFromClipboard(),
      onPasteImage: (image) => { void this.handleImagePaste(image); },
      onOpenUrl: (url) => { void machineIpc.fix({ kind: "openUrl", url }).catch(() => undefined); },
      onContextMenu: (request) => this.callbacks?.onContextMenu(request),
    });

    this.unsubscribeEvents = onTerminalEvent(sessionId, (event) => this.onStreamEvent(event));
    this.unsubscribeFont = subscribeTerminalFontSize(() => {
      this.engine.setFontSize(getTerminalFontSize());
    });
  }

  // ------------------------------------------------------------------- attach

  attach(
    slot: HTMLElement,
    options: { autoStart: boolean; scheme: ColorScheme; focus: boolean },
    callbacks: AttachCallbacks,
  ): () => void {
    this.attached = true;
    this.autoStart = options.autoStart;
    this.callbacks = callbacks;
    this.engine.setTheme(options.scheme);
    this.engine.setFontSize(getTerminalFontSize());
    this.engine.mount(slot);
    this.ptyResize.flush();
    if (options.focus) this.engine.focus();
    this.publish();

    if (this.connected) {
      void this.sync("entry");
    } else {
      this.phase = currentOfflinePhase();
      this.publish();
    }

    return () => {
      this.ptyResize.flush();
      this.attached = false;
      this.callbacks = null;
      this.published = undefined;
      this.engine.unmount();
    };
  }

  setScheme(scheme: ColorScheme) {
    this.engine.setTheme(scheme);
  }

  getViewState(scroll = this.engine.getScrollState()): TerminalViewState {
    return {
      phase: this.phase,
      status: this.replay.status,
      busy: this.busy,
      error: this.error,
      grid: this.grid,
      scroll,
    };
  }

  private publish(scroll?: ScrollState) {
    if (!this.callbacks) return;
    const next = this.getViewState(scroll);
    const previous = this.published;
    if (previous && previous.phase === next.phase && previous.status === next.status
      && previous.busy === next.busy && previous.error === next.error && previous.grid === next.grid
      && previous.scroll.viewportY === next.scroll.viewportY
      && previous.scroll.scrollbackLength === next.scroll.scrollbackLength
      && previous.scroll.rows === next.scroll.rows
      && previous.scroll.isAltScreen === next.scroll.isAltScreen
      && previous.scroll.hasUnreadBelow === next.scroll.hasUnreadBelow) return;
    this.published = next;
    this.callbacks.onState(next);
  }

  // --------------------------------------------------------------- connection

  handleConnectionChange(ready: boolean) {
    if (this.disposed) return;
    if (ready === this.connected) {
      if (!ready) {
        const nextPhase = currentOfflinePhase();
        if (nextPhase !== this.phase) {
          this.phase = nextPhase;
          this.publish();
        }
      }
      return;
    }
    this.connected = ready;
    if (ready) {
      this.phase = this.replay.generation != null ? "reconnecting" : "connecting";
      this.error = undefined;
      this.publish();
      if (this.attached) {
        void this.sync("reconnect");
      }
      return;
    }

    // Disconnected: preserve the terminal buffer & scrollback, cancel in-flight I/O,
    // and mark replay so a restarted daemon with a lower generation reconciles cleanly.
    this.connectionEpoch += 1;
    this.replay.markDisconnected();
    this.pending = [];
    this.pendingBytes = 0;
    this.busy = false;
    this.activeMode = undefined;
    this.queuedMode = undefined;
    this.resizing = false;
    this.sentSize = "";
    this.ptyResize.reset();
    this.inputQueue = "";
    this.inputInFlight = false;
    this.writeGeneration = undefined;
    this.phase = currentOfflinePhase();
    this.publish();
  }

  private onStreamEvent(event: TerminalStreamEvent) {
    if (this.disposed) return;
    if (event.type === "reconnect") {
      // Fresh host snapshot arrived after a guest link reconnect.
      this.connectionEpoch += 1;
      this.replay.markDisconnected();
      this.pending = [];
      this.pendingBytes = 0;
      this.busy = false;
      this.activeMode = undefined;
      this.resizing = false;
      this.sentSize = "";
      this.ptyResize.reset();
      this.inputQueue = "";
      this.inputInFlight = false;
      this.writeGeneration = undefined;
      this.connected = true;
      this.phase = "reconnecting";
      this.error = undefined;
      this.publish();
      if (this.attached) void this.sync("reconnect");
      return;
    }

    if (!this.connected) return;
    if (this.busy || this.error) {
      this.enqueuePending(event);
      return;
    }

    const delta = this.replay.event(event);
    if (delta === null) {
      this.enqueuePending(event);
      if (this.attached) void this.sync("stream");
      return;
    }

    if (delta) {
      this.engine.write(delta, { suppressResponses: false, reset: false, preserveViewport: true });
    }
    if (event.type === "terminalStatus") {
      this.publish();
    }
  }

  private enqueuePending(event: Extract<TerminalStreamEvent, { type: "sessionOutput" | "terminalStatus" }>) {
    const size = event.type === "sessionOutput" ? event.data.length : 0;
    if (this.pending.length >= PENDING_LIMIT || this.pendingBytes + size > PENDING_BYTES_LIMIT) {
      this.pending = [];
      this.pendingBytes = 0;
    }
    this.pending.push(event);
    this.pendingBytes += size;
  }

  /** Wait briefly for initial container measurement before spawning a new shell. */
  private waitForGrid(timeoutMs = 150): Promise<void> {
    if (this.gridMeasured || !this.attached) return Promise.resolve();
    return new Promise((resolve) => {
      const done = () => {
        clearTimeout(timer);
        resolve();
      };
      const timer = setTimeout(() => {
        const idx = this.gridReadyResolvers.indexOf(done);
        if (idx >= 0) this.gridReadyResolvers.splice(idx, 1);
        resolve();
      }, timeoutMs);
      this.gridReadyResolvers.push(done);
    });
  }

  async sync(mode: SyncMode) {
    if (this.disposed || !this.connected) return;
    if (this.busy) {
      // Upgrade an in-flight passive stream sync if a stronger sync was requested.
      if (this.activeMode === "stream" && mode !== "stream") {
        this.queuedMode = mode;
      } else if (mode === "retry" || mode === "reconnect") {
        this.queuedMode = mode;
      }
      this.publish();
      return;
    }

    this.busy = true;
    this.activeMode = mode;
    this.error = undefined;
    if (mode === "reconnect" || mode === "recover") {
      this.phase = "reconnecting";
    } else if (this.phase === "offline") {
      this.phase = "connecting";
    }
    this.publish();

    const epoch = this.connectionEpoch;
    const isCurrent = () => !this.disposed && epoch === this.connectionEpoch;

    try {
      if (mode !== "stream") {
        await this.waitForGrid();
        if (!isCurrent()) return;
      }

      let snapshot = await libraryIpc.terminalSnapshot(this.sessionId);
      if (!isCurrent()) return;

      const state = snapshot.status.state;
      const shouldStart =
        this.attached &&
        this.autoStart &&
        mode !== "stream" &&
        (state === "stopped" ||
          state === "exited" ||
          (state === "failed" && mode === "retry"));

      if (shouldStart) {
        snapshot = await libraryIpc.attachTerminal(
          this.sessionId,
          snapshot.generation,
          mode === "retry",
          this.grid.cols,
          this.grid.rows,
        );
      }
      if (!isCurrent()) return;

      const update = this.replay.snapshot(snapshot);
      if (update.reset || update.data) {
        this.engine.write(update.data, {
          suppressResponses: true,
          reset: update.reset,
          preserveViewport: true,
        });
      }
      this.phase = "connected";
    } catch (cause) {
      if (isCurrent()) {
        const ipc = toIpcError(cause);
        this.error = ipc.message;
        this.phase = appStore.get().guestReady ? "reconnecting" : currentOfflinePhase();
      }
    } finally {
      if (isCurrent()) {
        this.busy = false;
        this.activeMode = undefined;
        const nextMode = this.queuedMode;
        this.queuedMode = undefined;

        const queued = this.pending;
        this.pending = [];
        this.pendingBytes = 0;
        if (!this.error) {
          for (const event of queued) this.onStreamEvent(event);
        }

        this.publish();
        if (!this.error) {
          this.pushResize(this.grid);
          this.drainInput();
        }
        if (nextMode && this.attached && !this.error) {
          void this.sync(nextMode);
        }
      }
    }
  }

  // ------------------------------------------------------------------ PTY I/O

  private isRunning(): boolean {
    return !this.disposed && this.connected && this.replay.status?.state === "running";
  }

  private pushResize(grid: Grid) {
    this.grid = grid;
    const generation = this.replay.generation;
    const key = `${generation}:${grid.cols}x${grid.rows}`;
    if (generation == null || !this.isRunning() || this.busy || this.resizing || this.sentSize === key) {
      return;
    }
    this.resizing = true;
    const epoch = this.connectionEpoch;
    void libraryIpc
      .resizeSession(this.sessionId, generation, grid.cols, grid.rows)
      .then(() => {
        if (epoch !== this.connectionEpoch) return;
        this.sentSize = key;
      })
      .catch(() => {
        if (epoch !== this.connectionEpoch) return;
        // Leave `sentSize` unset so the size will be retried on next resize or recovery.
        this.sentSize = "";
      })
      .finally(() => {
        if (epoch !== this.connectionEpoch) return;
        this.resizing = false;
        const latestKey = `${this.replay.generation}:${this.grid.cols}x${this.grid.rows}`;
        if (latestKey !== this.sentSize && this.sentSize !== "") {
          this.pushResize(this.grid);
        }
      });
  }

  private sendInput(data: string, generation = this.replay.generation, epoch = this.connectionEpoch) {
    if (
      !data ||
      this.disposed ||
      !this.connected ||
      this.error ||
      epoch !== this.connectionEpoch
    ) {
      return;
    }
    // During a transient snapshot sync on a running generation, keep typed keys
    // buffered instead of silently dropping them.
    if (this.busy) {
      if (this.replay.status?.state === "running" && this.inputQueue.length + data.length <= INPUT_QUEUE_LIMIT) {
        this.inputQueue += data;
      }
      return;
    }
    if (generation == null || !this.isRunning() || generation !== this.replay.generation) {
      return;
    }
    if (this.writeGeneration !== generation) {
      this.writeGeneration = generation;
      this.inputQueue = "";
    }
    if (this.inputQueue.length + data.length <= INPUT_QUEUE_LIMIT) {
      this.inputQueue += data;
    }
    this.drainInput();
  }

  private drainInput() {
    if (this.inputInFlight || !this.inputQueue || this.busy || this.error || !this.isRunning()) {
      return;
    }
    const generation = this.replay.generation;
    if (generation == null) return;
    if (this.writeGeneration !== undefined && this.writeGeneration !== generation) {
      this.inputQueue = "";
      this.writeGeneration = generation;
      return;
    }
    this.writeGeneration = generation;
    const epoch = this.connectionEpoch;
    const batch = this.inputQueue;
    this.inputQueue = "";
    this.inputInFlight = true;

    void libraryIpc
      .writeSession(this.sessionId, generation, batch)
      .catch((cause: unknown) => {
        if (this.disposed || epoch !== this.connectionEpoch) return;
        const ipc = toIpcError(cause);
        // If the generation ended or connection dropped mid-write, reconcile right away.
        if (ipc.code === "conflict" || ipc.code === "unavailable") {
          this.inputQueue = "";
          this.replay.markDisconnected();
          this.sentSize = "";
          this.ptyResize.reset();
          if (this.attached && this.connected) {
            void this.sync("recover");
          } else {
            this.phase = currentOfflinePhase();
            this.publish();
          }
        }
      })
      .finally(() => {
        if (epoch !== this.connectionEpoch) return;
        this.inputInFlight = false;
        if (!this.disposed && this.inputQueue) {
          this.drainInput();
        }
      });
  }

  private async handleImagePaste(image: Blob) {
    const preview = URL.createObjectURL(image);
    const size = formatBytes(image.size);
    if (image.size > PASTE_IMAGE_MAX) {
      uiActions.toast("Image too large to paste", "error", {
        image: preview,
        detail: `${size} · the limit is 20 MB`,
      });
      return;
    }
    const toast = uiActions.toast("Attaching image…", "loading", {
      image: preview,
      detail: `${size} · copying into the machine`,
    });
    const generation = this.replay.generation;
    const epoch = this.connectionEpoch;
    try {
      const data = await blobBase64(image);
      const { path } = await libraryIpc.terminalPasteImage(this.sessionId, image.type, data);
      const text = `${typedPath(path)} `;
      if (generation != null && epoch === this.connectionEpoch && generation === this.replay.generation) {
        this.engine.pasteText(text);
      }
      uiActions.updateToast(toast, {
        tone: "success",
        message: "Image attached",
        detail: `${path.split("/").pop()} · ${size}`,
      });
    } catch (cause) {
      uiActions.updateToast(toast, {
        tone: "error",
        message: "The image was not pasted",
        detail: toIpcError(cause).message,
      });
    }
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.ptyResize.cancel();
    this.pending = [];
    this.unsubscribeEvents();
    this.unsubscribeFont();
    this.engine.dispose();
  }
}

function currentOfflinePhase(): ConnectionPhase {
  const conn = appStore.get().machine?.connection;
  return conn === "connecting" || conn === "updating" ? "reconnecting" : "offline";
}

async function copyToClipboard(text: string): Promise<void> {
  try {
    await clipboard.write(text);
    return;
  } catch {
    // Fallback for non-Tauri browser environments (e.g. UI smoke tests).
  }
  try {
    await navigator.clipboard?.writeText(text);
  } catch {
    // Ignore clipboard permission errors.
  }
}

async function readFromClipboard(): Promise<string | null> {
  try {
    const text = await clipboard.read();
    if (text) return text;
  } catch {
    // Fallback below.
  }
  try {
    return (await navigator.clipboard?.readText()) ?? null;
  } catch {
    return null;
  }
}

// ----------------------------------------------------------------------- pool

const pool = new Map<string, TerminalSessionController>();
let storeWatched = false;

function ensureStoreWatcher() {
  if (storeWatched) return;
  storeWatched = true;
  let lastReady = appStore.get().guestReady;
  let lastConn = appStore.get().machine?.connection;

  appStore.subscribe(() => {
    const state = appStore.get();
    const ids = new Set(state.sessions.map((session) => session.id));
    const readyChanged = state.guestReady !== lastReady;
    const connChanged = state.machine?.connection !== lastConn;

    if (readyChanged || connChanged) {
      lastReady = state.guestReady;
      lastConn = state.machine?.connection;
      for (const controller of pool.values()) {
        if (ids.has(controller.sessionId)) {
          controller.handleConnectionChange(state.guestReady);
        }
      }
    }

    // Dispose detached entries ONLY when the session was actually deleted (or machine wiped),
    // NOT during a transient guest disconnect — so scrollback survives reconnects.
    const machineWiped = state.machine?.state === "notInstalled";
    if (state.guestReady || machineWiped) {
      for (const [id, controller] of pool) {
        if (!controller.attached && (machineWiped || !ids.has(id))) {
          controller.dispose();
          pool.delete(id);
        }
      }
    }
  });
}

function evictPool() {
  for (const [id, controller] of pool) {
    if (pool.size <= POOL_LIMIT) break;
    if (!controller.attached) {
      controller.dispose();
      pool.delete(id);
    }
  }
}

export async function acquireTerminalSession(
  sessionId: string,
  scheme: ColorScheme,
): Promise<TerminalSessionController> {
  ensureStoreWatcher();
  const [ghostty] = await Promise.all([loadGhostty(), loadTerminalFonts()]);
  let controller = pool.get(sessionId);
  if (!controller) {
    controller = new TerminalSessionController(ghostty, sessionId, scheme);
  } else {
    controller.setScheme(scheme);
  }
  // Refresh LRU recency.
  pool.delete(sessionId);
  pool.set(sessionId, controller);
  evictPool();
  return controller;
}

export function releaseUnusedTerminals() {
  const state = appStore.get();
  const ids = new Set(state.sessions.map((session) => session.id));
  if (state.guestReady || state.machine?.state === "notInstalled") {
    for (const [id, controller] of pool) {
      if (!controller.attached && !ids.has(id)) {
        controller.dispose();
        pool.delete(id);
      }
    }
  }
  evictPool();
}
