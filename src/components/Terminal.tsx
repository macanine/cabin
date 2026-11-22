import { useAutoRefresh } from "../lib/useAutoRefresh";
import { Button, useComputedColorScheme } from "@mantine/core";
import { useEffect, useRef, useState } from "react";
import type { Terminal as GhosttyTerminal } from "ghostty-web";
import { toIpcError } from "../services/ipc/common";
import { libraryIpc } from "../services/ipc/library";
import { clipboard } from "../services/ipc/dialogs";
import { appStore, onTerminalEvent } from "../state/store";
import { cellAt, WHEEL_DOWN, WHEEL_UP, WheelAccumulator, wheelReport } from "../lib/terminalInput";
import { type ColorScheme, terminalColors } from "../lib/terminalTheme";
import { gridFor, settledSender, type Grid } from "../lib/terminalFit";
import { blobBase64, PASTE_IMAGE_MAX, pastedImage, typedPath } from "../lib/terminalPaste";
import { formatBytes } from "../lib/format";
import { uiActions } from "../state/ui";
import { TerminalReplay, type TerminalEvent } from "../lib/terminalReplay";
import { captureViewport, restoreViewport } from "../lib/terminalViewport";
import type { SessionStatus } from "../services/ipc/generated/core";

export type TerminalSize = Grid;

// Load Ghostty on first terminal entry and allow initialization to be retried.
let ghosttyReady: Promise<typeof GhosttyTerminal> | undefined;
function ghostty(): Promise<typeof GhosttyTerminal> {
  ghosttyReady ??= import("ghostty-web").then(async ({ init, Terminal }) => {
    await init();
    return Terminal;
  }).catch((error: unknown) => {
    ghosttyReady = undefined;
    throw error;
  });
  return ghosttyReady;
}

function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** Focus the terminal unless the user is typing in another field. */
function focusUnlessEditing(term: GhosttyTerminal) {
  const active = document.activeElement as HTMLElement | null;
  const editing =
    active != null &&
    active !== document.body &&
    !active.closest(".terminal-host") &&
    (active.matches("input, textarea, select, [contenteditable='true']") || active.closest("[role='dialog'], [role='menu']"));
  if (!editing && active !== term.textarea) term.focus();
}

// ------------------------------------------------------------------- pool
//
// Pooled emulators keep receiving output in the background. A new emulator
// or a gap in the live stream is recovered from the daemon's bounded history.
const POOL_LIMIT = 8;
const VIEWPORT_LIMIT = 64;
const PENDING_LIMIT = 1024;

interface TerminalViewState {
  status?: SessionStatus;
  busy: boolean;
  error?: string;
}

interface Entry {
  sessionId: string;
  scheme: ColorScheme;
  /** Persistent element the terminal lives in; moved between views. */
  root: HTMLDivElement;
  term: GhosttyTerminal;
  attached: boolean;
  autoStart: boolean;
  onState?: (next: TerminalViewState) => void;
  resize: (grid: Grid) => void;
  sync: (mode: "entry" | "retry" | "stream") => void;
  connection: (ready: boolean) => void;
  dispose: () => void;
}

/** Insertion order is recency: the last entry is the most recent. */
const pool = new Map<string, Entry>();
const viewports = new Map<string, number>();

function rememberViewport(id: string, offset: number) {
  viewports.delete(id);
  if (offset > 0) viewports.set(id, offset);
  if (viewports.size > VIEWPORT_LIMIT) {
    const oldest = viewports.keys().next().value;
    if (oldest !== undefined) viewports.delete(oldest);
  }
}

function evict() {
  for (const [id, entry] of pool) {
    if (pool.size <= POOL_LIMIT) break;
    if (!entry.attached) {
      entry.dispose();
      pool.delete(id);
    }
  }
}

function disposeWhere(test: (entry: Entry) => boolean) {
  for (const [id, entry] of pool) {
    if (!entry.attached && test(entry)) {
      entry.dispose();
      pool.delete(id);
    }
  }
}

let watching = false;
function watchStore() {
  if (watching) return;
  watching = true;
  let ready = appStore.get().guestReady;
  appStore.subscribe(() => {
    const state = appStore.get();
    const ids = new Set(state.sessions.map((session) => session.id));
    if (ready !== state.guestReady) {
      ready = state.guestReady;
      for (const entry of pool.values()) {
        if (entry.attached && ids.has(entry.sessionId)) entry.connection(ready);
      }
    }
    disposeWhere((entry) => !ready || !ids.has(entry.sessionId));
    if (state.guestReady || state.machine?.state === "notInstalled") {
      for (const id of viewports.keys()) if (!ids.has(id)) viewports.delete(id);
    }
  });
}

function acquire(Terminal: typeof GhosttyTerminal, sessionId: string, scheme: ColorScheme, slot: HTMLElement): Entry {
  watchStore();
  let entry = pool.get(sessionId);
  if (entry && entry.scheme !== scheme) {
    entry.dispose();
    pool.delete(sessionId);
    entry = undefined;
  }
  entry ??= create(Terminal, sessionId, scheme, slot);
  pool.delete(sessionId);
  pool.set(sessionId, entry);
  return entry;
}

/** Row height as a factor of the font size: the density of a native
 *  terminal. Taller rows open gaps in box-drawing lines (agent UIs). */
const LINE_HEIGHT = 1.2;

function create(Terminal: typeof GhosttyTerminal, sessionId: string, scheme: ColorScheme, slot: HTMLElement): Entry {
  const root = document.createElement("div");
  root.className = "terminal-host";
  // Open while in the document so font metrics and the first fit are real.
  slot.appendChild(root);

  const term = new Terminal({
    fontFamily: cssVar("--mono") || "monospace",
    fontSize: 13,
    cursorBlink: true,
    scrollback: 10000,
    // Scroll position is restored after every write (see `write` below); an
    // easing animation would keep pulling against that.
    smoothScrollDuration: 0,
    theme: terminalColors(scheme, {
      background: cssVar("--terminal-bg"),
      foreground: cssVar("--terminal-fg"),
      accent: cssVar("--accent"),
    }),
  });
  // WebKit's IME edits a contenteditable host itself, inserting nodes before
  // the canvas. Keep all text input in Ghostty's textarea instead, and let
  // attach() decide when the terminal should take focus.
  let opened = false;
  let positionInput = (_force = false) => {};
  term.focus = () => {
    if (opened) {
      positionInput(true);
      term.textarea?.focus({ preventScroll: true });
    }
  };
  term.open(root);
  opened = true;
  // Rows at LINE_HEIGHT (vite.config.ts patches the renderer to honour it),
  // measured again once the web font has loaded; the fit loop picks the new
  // cell size up on its next frame.
  const remeasure = () => {
    const renderer = term.renderer as (NonNullable<typeof term.renderer> & { lineHeight?: number }) | undefined;
    if (!renderer) return;
    renderer.lineHeight = LINE_HEIGHT;
    renderer.remeasureFont();
  };
  remeasure();
  void document.fonts?.ready.then(remeasure);
  root.contentEditable = "false";
  root.removeAttribute("tabindex");
  root.removeAttribute("role");
  root.removeAttribute("aria-label");
  root.removeAttribute("aria-multiline");
  const textarea = term.textarea;
  if (textarea) {
    // Keep a real caret rectangle for the IME even while the input is hidden.
    textarea.style.clipPath = "none";
    textarea.style.pointerEvents = "none";
    let composing = false;
    let preedit = "";
    let lastCommit = "";
    let commitTimer: ReturnType<typeof setTimeout> | undefined;
    const measure = document.createElement("canvas").getContext("2d");
    if (measure) measure.font = `${term.options.fontSize}px ${term.options.fontFamily}`;
    textarea.wrap = "off";
    positionInput = (force = false) => {
      const cursor = term.wasmTerm?.getCursor();
      const metrics = term.renderer?.getMetrics();
      // Ghostty temporarily moves this textarea for its context menu.
      if (!cursor || !metrics || (!force && textarea.style.position === "fixed")) return;
      const col = Math.max(0, Math.min(term.cols - 1, cursor.x));
      const row = Math.max(0, Math.min(term.rows - 1, cursor.y + Math.floor(term.viewportY)));
      const textWidth = composing ? (measure?.measureText(preedit).width ?? preedit.length * metrics.width) : 0;
      const width = Math.min((term.cols - col) * metrics.width, Math.max(metrics.width, Math.ceil(textWidth + metrics.width)));
      textarea.style.position = "absolute";
      textarea.style.left = `${col * metrics.width}px`;
      textarea.style.top = `${row * metrics.height}px`;
      textarea.style.width = `${width}px`;
      textarea.style.height = `${metrics.height}px`;
      textarea.style.font = `${term.options.fontSize}px ${term.options.fontFamily}`;
      textarea.style.lineHeight = `${metrics.height}px`;
    };
    // Ghostty prevents beforeinput on the host; it must not cancel the IME's
    // composition events on the textarea. compositionend still bubbles to
    // Ghostty, which sends the committed text to the PTY once.
    textarea.addEventListener("beforeinput", (event) => event.stopPropagation());
    textarea.addEventListener("compositionstart", () => {
      composing = true;
      preedit = "";
      lastCommit = "";
      clearTimeout(commitTimer);
      textarea.classList.add("terminal-composing");
      positionInput(true);
    });
    textarea.addEventListener("compositionupdate", (event) => {
      preedit = event.data;
      positionInput(true);
    });
    textarea.addEventListener("compositionend", (event) => {
      composing = false;
      preedit = "";
      lastCommit = event.data;
      textarea.classList.remove("terminal-composing");
      positionInput();
      clearTimeout(commitTimer);
      commitTimer = setTimeout(() => { lastCommit = ""; }, 0);
      queueMicrotask(() => { if (!composing) textarea.value = ""; });
    });
    // Some input methods commit text without a composition event or keydown.
    textarea.addEventListener("input", (event) => {
      const input = event as InputEvent;
      if (composing || input.isComposing || !["insertText", "insertReplacementText"].includes(input.inputType)) return;
      const text = input.data ?? textarea.value;
      if (text && text !== lastCommit) term.input(text, true);
      lastCommit = "";
      textarea.value = "";
    });
    positionInput();
  }
  // Full reset (RIS) before anything is shown. ghostty-web's WASM build
  // reuses the memory of disposed terminals without clearing it, so a new
  // terminal's untouched rows showed text from other sessions. RIS rewrites
  // every cell, scrollback page and mode; clearing the screen (ED 2) is not
  // enough once the terminal scrolls.
  term.write("\x1bc");

  let replay = new TerminalReplay();
  let connected = appStore.get().guestReady;
  let connectionEpoch = 0;
  let disposed = false;
  let busy = false;
  let restoringViewport = viewports.get(sessionId) ?? 0;
  let activeMode: "entry" | "retry" | "stream" | undefined;
  let syncAgain: "entry" | "retry" | undefined;
  let replaying = false;
  let error: string | undefined;
  let grid = { cols: term.cols, rows: term.rows };
  let pending: TerminalEvent[] = [];
  let pendingSize = 0;
  let sentSize = "";
  let resizing = false;
  let writes = Promise.resolve();
  let writeGeneration: number | undefined;

  // Ghostty scrolls to the bottom on write and emits terminal replies
  // synchronously. Keep the reader's place and suppress replies to history.
  const write = (data: string, history = false, reset = false) => {
    const offset = term.viewportY || (reset ? restoringViewport : 0);
    if (reset) restoringViewport = 0;
    const before = term.getScrollbackLength();
    const line = (index: number) => term.buffer.normal.getLine(index)?.translateToString(true);
    const anchor = !reset && term.buffer.active.type === "normal" ? captureViewport(before, offset, line) : null;
    replaying = history;
    try {
      if (reset) term.write("\x1bc");
      if (data) term.write(data);
    } finally {
      replaying = false;
    }
    const after = term.getScrollbackLength();
    if (offset > 0 && after > 0 && term.buffer.active.type === "normal") {
      term.scrollToLine(anchor ? restoreViewport(anchor, after, line) : Math.min(after, offset));
    }
    positionInput();
  };

  const publish = () => entry.onState?.({ status: replay.status, busy, error });
  const running = () => !disposed && connected && replay.status?.state === "running";
  const resize = () => {
    const generation = replay.generation;
    const key = `${generation}:${grid.cols}x${grid.rows}`;
    if (generation == null || !running() || busy || resizing || sentSize === key) return;
    sentSize = key;
    resizing = true;
    const epoch = connectionEpoch;
    void libraryIpc.resizeSession(sessionId, generation, grid.cols, grid.rows).catch(() => undefined).finally(() => {
      if (epoch !== connectionEpoch) return;
      resizing = false;
      resize();
    });
  };
  const sendInput = (data: string, generation = replay.generation, epoch = connectionEpoch) => {
    if (replaying || generation == null || !running() || busy || error ||
        epoch !== connectionEpoch || generation !== replay.generation) return;
    if (writeGeneration !== generation) {
      writeGeneration = generation;
      writes = Promise.resolve();
    }
    writes = writes.then(async () => {
      if (running() && epoch === connectionEpoch && generation === replay.generation) {
        await libraryIpc.writeSession(sessionId, generation, data);
      }
    }).catch(() => undefined);
  };
  const queue = (event: TerminalEvent) => {
    const size = event.type === "sessionOutput" ? event.data.length : 0;
    if (pending.length >= PENDING_LIMIT || pendingSize + size > 512 * 1024) {
      pending = [];
      pendingSize = 0;
    }
    pending.push(event);
    pendingSize += size;
  };
  const receive = (event: TerminalEvent) => {
    if (!connected) return;
    if (busy || error) {
      queue(event);
      return;
    }
    const data = replay.event(event);
    if (data == null) {
      queue(event);
      if (entry.attached) entry.sync("stream");
      return;
    }
    if (data) write(data);
    if (event.type === "terminalStatus") publish();
  };
  const sync = async (mode: "entry" | "retry" | "stream") => {
    if (disposed || !connected) return;
    if (busy) {
      if (activeMode === "stream" && mode !== "stream") syncAgain = mode;
      publish();
      return;
    }
    busy = true;
    activeMode = mode;
    error = undefined;
    publish();
    const epoch = connectionEpoch;
    const current = () => !disposed && epoch === connectionEpoch;
    try {
      let snapshot = await libraryIpc.terminalSnapshot(sessionId);
      if (!current()) return;
      const state = snapshot.status.state;
      if (entry.attached && entry.autoStart && mode !== "stream" &&
          (state === "stopped" || state === "exited" || (state === "failed" && mode === "retry"))) {
        snapshot = await libraryIpc.attachTerminal(sessionId, snapshot.generation, mode === "retry", grid.cols, grid.rows);
      }
      if (!current()) return;
      const update = replay.snapshot(snapshot);
      if (update.reset || update.data) write(update.data, true, update.reset);
    } catch (cause) {
      if (current()) error = toIpcError(cause).message;
    } finally {
      if (current()) {
        busy = false;
        activeMode = undefined;
        const nextMode = syncAgain;
        syncAgain = undefined;
        if (nextMode && entry.attached && !error) entry.sync(nextMode);
        const events = pending;
        pending = [];
        pendingSize = 0;
        for (const event of events) receive(event);
        publish();
        resize();
      }
    }
  };
  const unsubscribe = onTerminalEvent(sessionId, receive);
  const input = term.onData(sendInput);
  const inputResize = term.onResize(() => positionInput());
  const inputScroll = term.onScroll(() => positionInput());

  // ghostty-web: returning true means "handled, skip default handling".
  term.attachCustomKeyEventHandler((event) => {
    if (event.type !== "keydown" || !event.ctrlKey || !event.shiftKey) return false;
    // App shortcuts (handled on window): keep them away from the PTY.
    if (["KeyN", "KeyK", "KeyB"].includes(event.code)) return true;
    if (event.code === "KeyC") {
      const selection = term.getSelection();
      if (selection) void clipboard.write(selection);
      return true;
    }
    if (event.code === "KeyV") {
      const generation = replay.generation;
      const epoch = connectionEpoch;
      void clipboard
        .read()
        .then((text) => {
          if (text && generation != null) sendInput(text, generation, epoch);
        })
        .catch(() => undefined);
      return true;
    }
    return false;
  });

  // An image in a paste (⌘V) is saved in the machine and its path pasted:
  // the agents cannot read the Mac's clipboard. Text pastes are untouched.
  const onPaste = (event: ClipboardEvent) => {
    const image = pastedImage(event.clipboardData?.items);
    if (!image) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    const preview = URL.createObjectURL(image);
    const size = formatBytes(image.size);
    if (image.size > PASTE_IMAGE_MAX) {
      uiActions.toast("Image too large to paste", "error", { image: preview, detail: `${size} · the limit is 20 MB` });
      return;
    }
    const toast = uiActions.toast("Attaching image…", "loading", { image: preview, detail: `${size} · copying into the machine` });
    const generation = replay.generation;
    const epoch = connectionEpoch;
    void blobBase64(image)
      .then((data) => libraryIpc.terminalPasteImage(sessionId, image.type, data))
      .then(({ path }) => {
        // As a real paste when the program asked for bracketed paste, so
        // Claude Code turns the path into an image attachment.
        const text = `${typedPath(path)} `;
        sendInput(term.getMode(2004) ? `\x1b[200~${text}\x1b[201~` : text, generation, epoch);
        uiActions.updateToast(toast, { tone: "success", message: "Image attached", detail: `${path.split("/").pop()} · ${size}` });
      })
      .catch((cause: unknown) => uiActions.updateToast(toast, {
        tone: "error", message: "The image was not pasted", detail: toIpcError(cause).message,
      }));
  };
  root.addEventListener("paste", onPaste, true);

  // Wheel: when the agent's UI tracks the mouse, send it real wheel reports
  // at the pointer (ghostty-web would send ↑/↓ keys). Otherwise the default
  // scrolls the scrollback. Shift+wheel always scrolls the local scrollback.
  const wheel = new WheelAccumulator();
  term.attachCustomWheelEventHandler((event) => {
    if (event.shiftKey || !term.hasMouseTracking()) return false;
    const canvas = root.querySelector("canvas");
    if (!canvas) return false;
    const rect = canvas.getBoundingClientRect();
    const notches = wheel.take(event.deltaY, event.deltaMode, rect.height / term.rows, term.rows);
    if (notches === 0) return true;
    const { col, row } = cellAt(event.clientX, event.clientY, rect, term.cols, term.rows);
    const report = wheelReport(notches < 0 ? WHEEL_UP : WHEEL_DOWN, col, row, term.getMode(1006));
    sendInput(report.repeat(Math.abs(notches)));
    return true;
  });

  const entry: Entry = {
    sessionId,
    scheme,
    root,
    term,
    attached: false,
    autoStart: false,
    resize: (next) => {
      grid = next;
      resize();
    },
    sync: (mode) => void sync(mode),
    connection: (ready) => {
      connected = ready;
      if (ready) {
        entry.sync("entry");
        return;
      }
      connectionEpoch += 1;
      replay = new TerminalReplay();
      pending = [];
      pendingSize = 0;
      busy = false;
      activeMode = undefined;
      syncAgain = undefined;
      resizing = false;
      sentSize = "";
      writeGeneration = undefined;
    },
    dispose: () => {
      disposed = true;
      pending = [];
      rememberViewport(sessionId, term.viewportY || restoringViewport);
      root.removeEventListener("paste", onPaste, true);
      input.dispose();
      inputResize.dispose();
      inputScroll.dispose();
      unsubscribe();
      term.dispose();
      root.remove();
    },
  };
  return entry;
}

/** Show a pooled terminal in `slot`; returns the detach function.
 *
 * Sizing (see lib/terminalFit.ts): the emulator is fitted to the slot on
 * every animation frame the slot changes size, so the grid always fills it;
 * the PTY is resized once the size has settled (and right away on attach, so
 * a program starts at the real size). ghostty-web's FitAddon is not used: it
 * ignored resizes for 50 ms after each one (dropping the final size of a
 * window drag) and reserved 15 px for a scrollbar that is drawn as an
 * overlay. */
function attach(entry: Entry, slot: HTMLElement, onSize: (size: TerminalSize) => void, focusOnAttach: boolean): () => void {
  const { term, root, sessionId } = entry;
  entry.attached = true;
  if (root.parentElement !== slot) slot.appendChild(root);
  evict();

  const pty = settledSender((grid) => {
    onSize(grid);
    entry.resize(grid);
  });
  const fitNow = () => {
    const metrics = term.renderer?.getMetrics();
    const grid = metrics && gridFor(slot.clientWidth, slot.clientHeight, metrics);
    if (!grid) return;
    if (grid.cols !== term.cols || grid.rows !== term.rows) term.resize(grid.cols, grid.rows);
    pty.push(grid);
  };
  let frame = 0;
  const observer = new ResizeObserver(() => {
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(fitNow);
  });
  observer.observe(slot);
  fitNow();
  pty.flush();

  // Coming back to the window: give the terminal its focus back unless
  // something else was focused on purpose.
  const onWindowFocus = () => requestAnimationFrame(() => focusUnlessEditing(term));
  window.addEventListener("focus", onWindowFocus);
  if (focusOnAttach) term.focus();

  return () => {
    window.removeEventListener("focus", onWindowFocus);
    observer.disconnect();
    cancelAnimationFrame(frame);
    // A size still settling belongs to the view being left: send it now.
    pty.flush();
    entry.attached = false;
    entry.onState = undefined;
    root.remove();
    const state = appStore.get();
    if (!state.guestReady || !state.sessions.some((session) => session.id === sessionId)) {
      entry.dispose();
      pool.delete(sessionId);
    }
    evict();
  };
}

// -------------------------------------------------------------- component

/** A Ghostty terminal (ghostty-web) bound to one backend session: streams
 * output, forwards input and keeps the PTY size in sync. Emulators are pooled
 * per session (see above).
 *
 * `focusKey`: whenever it changes (e.g. the session starts) the terminal takes
 * keyboard focus again, so typing reaches the agent instead of the button
 * that was just clicked. */
export function SessionTerminal({
  sessionId,
  onSize,
  focusKey,
  plain = false,
  archived = false,
}: {
  sessionId: string;
  onSize?: (size: TerminalSize) => void;
  focusKey?: unknown;
  plain?: boolean;
  archived?: boolean;
}) {
  const slot = useRef<HTMLDivElement>(null);
  const entryRef = useRef<Entry | null>(null);
  const [state, setState] = useState<TerminalViewState>({ busy: true });
  const [loadAttempt, setLoadAttempt] = useState(0);
  const onSizeRef = useRef(onSize);
  onSizeRef.current = onSize;
  // Colours follow the app (system) scheme; a change rebuilds the emulator.
  const scheme = useComputedColorScheme("light", { getInitialValueInEffect: false });

  useEffect(() => {
    const term = entryRef.current?.term;
    if (term) focusUnlessEditing(term);
  }, [focusKey]);

  useEffect(() => {
    const element = slot.current;
    if (!element) return;
    // Selecting a session should move focus from the previous view, including
    // a sidebar search field. Do not steal it if the user moved it elsewhere
    // while the terminal was loading.
    const focusAtNavigation = document.activeElement;
    let disposed = false;
    let detach = () => {};
    void ghostty().then((Terminal) => {
      if (disposed) return;
      const active = document.activeElement;
      const focusOnAttach =
        (active === focusAtNavigation || active === document.body || active == null) &&
        !active?.closest("[role='dialog'], [role='menu']");
      const entry = acquire(Terminal, sessionId, scheme, element);
      entryRef.current = entry;
      entry.autoStart = plain && !archived;
      entry.onState = setState;
      detach = attach(entry, element, (size) => onSizeRef.current?.(size), focusOnAttach);
      entry.sync("entry");
    }).catch((cause: unknown) => {
      if (!disposed) setState({ busy: false, error: toIpcError(cause).message });
    });
    return () => {
      disposed = true;
      entryRef.current = null;
      detach();
    };
  }, [sessionId, scheme, plain, archived, loadAttempt]);

  const retry = () => {
    const entry = entryRef.current;
    if (entry) {
      entry.term.focus();
      entry.sync("retry");
    } else {
      setState({ busy: true });
      setLoadAttempt((attempt) => attempt + 1);
    }
  };
  useAutoRefresh(() => {
    const entry = entryRef.current;
    if (entry) entry.sync("stream");
    else setLoadAttempt((attempt) => attempt + 1);
  }, { enabled: !!state.error, initial: false, interval: 8_000 });
  const failed = state.error ?? (plain && state.status?.state === "failed" ? state.status.message : null);
  const exited = plain && !archived && state.status?.state === "exited" ? state.status : null;

  return (
    <>
      <div
        className="terminal-frame"
        // Clicks on the padding around the canvas still focus the terminal.
        onMouseDown={(event) => {
          const term = entryRef.current?.term;
          if (event.button === 0 &&
              (event.target === event.currentTarget || event.target === slot.current || event.target === term?.element)) {
            event.preventDefault();
            term?.focus();
          }
        }}
      >
        <div className="terminal-slot" ref={slot} />
      </div>
      {state.busy ? plain && state.status?.state !== "running" && (
        <div className="terminal-banner" role="status">Opening terminal…</div>
      ) : failed ? (
        <div className="terminal-banner" role="status">
          <span>{failed}</span>
          {plain && state.status?.state === "failed" && <Button size="xs" variant="default" onClick={retry}>Restart shell</Button>}
        </div>
      ) : exited ? (
        <div className="terminal-banner" role="status">
          <span>
            {exited.code === 0 ? "Shell exited." : exited.code == null ? "Shell ended." : `Shell exited with code ${exited.code}.`}
            {" Reopen this terminal to start another shell."}
          </span>
        </div>
      ) : archived && plain && (
        <div className="terminal-banner" role="status">Archived terminal</div>
      )}
    </>
  );
}
