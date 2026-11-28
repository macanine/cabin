/**
 * Pure terminal protocol primitives: stream replay reconciliation, sizing,
 * scrollback viewport anchoring, keyboard/mouse encoding, and paste helpers.
 * No DOM or React dependencies so Node tests can exercise everything directly.
 */
import type { AppEvent, SessionStatus, TerminalSnapshot } from "../../services/ipc/generated/core";

export type TerminalEvent = Extract<AppEvent, { type: "sessionOutput" | "terminalStatus" }>;
export type TerminalStreamEvent = TerminalEvent | { type: "reconnect" };

// ------------------------------------------------------------- OSC 8 sanitizer

const OSC8_MAX_BODY = 4096;
const OSC8_TEXT = 0;
const OSC8_ESC = 1;
const OSC8_OSC = 2;
const OSC8_EIGHT = 3;
const OSC8_DROP = 4;
const OSC8_DROP_ESC = 5;

/**
 * Streaming filter that strips OSC 8 hyperlink metadata (`ESC ] 8 ; ... BEL/ST`)
 * while preserving visible link text, SGR styling, and all other OSC sequences.
 *
 * `ghostty-web@0.4.0`'s compiled `libghostty` WASM corrupts its internal
 * `PageList` linked list when `ghostty_terminal_resize` reflows wrapped rows
 * carrying OSC 8 hyperlink metadata, causing subsequent render/resize calls to
 * spin at 100% CPU. Cabin detects clickable URLs directly from cell text and
 * does not read `hyperlink_id`, so stripping OSC 8 at the WASM boundary avoids
 * the upstream WASM bug cleanly across arbitrary chunk splits.
 */
export class Osc8Stripper {
  private state = OSC8_TEXT;
  private pending = "";
  private dropped = 0;

  reset() {
    this.state = OSC8_TEXT;
    this.pending = "";
    this.dropped = 0;
  }

  push(chunk: string): string {
    if (!chunk) return "";
    if (
      this.state === OSC8_TEXT &&
      chunk.indexOf("\x1b") === -1 &&
      chunk.indexOf("\x9d") === -1
    ) {
      return chunk;
    }

    let out = "";
    let copyStart = this.state === OSC8_TEXT ? 0 : -1;

    for (let i = 0; i < chunk.length; i++) {
      const code = chunk.charCodeAt(i);

      if (this.state === OSC8_TEXT) {
        if (code === 0x1b || code === 0x9d) {
          if (copyStart >= 0 && i > copyStart) out += chunk.slice(copyStart, i);
          copyStart = -1;
          this.pending = chunk[i];
          this.state = code === 0x1b ? OSC8_ESC : OSC8_OSC;
        }
        continue;
      }

      if (this.state === OSC8_ESC) {
        if (code === 0x5d) {
          this.pending += "]";
          this.state = OSC8_OSC;
        } else {
          out += this.pending;
          this.pending = "";
          if (code === 0x1b || code === 0x9d) {
            this.pending = chunk[i];
            this.state = code === 0x1b ? OSC8_ESC : OSC8_OSC;
          } else {
            this.state = OSC8_TEXT;
            copyStart = i;
          }
        }
        continue;
      }

      if (this.state === OSC8_OSC) {
        if (code === 0x38) {
          this.pending += "8";
          this.state = OSC8_EIGHT;
        } else {
          out += this.pending;
          this.pending = "";
          if (code === 0x1b || code === 0x9d) {
            this.pending = chunk[i];
            this.state = code === 0x1b ? OSC8_ESC : OSC8_OSC;
          } else {
            this.state = OSC8_TEXT;
            copyStart = i;
          }
        }
        continue;
      }

      if (this.state === OSC8_EIGHT) {
        if (code === 0x3b) {
          this.pending = "";
          this.dropped = 0;
          this.state = OSC8_DROP;
        } else {
          out += this.pending;
          this.pending = "";
          if (code === 0x1b || code === 0x9d) {
            this.pending = chunk[i];
            this.state = code === 0x1b ? OSC8_ESC : OSC8_OSC;
          } else {
            this.state = OSC8_TEXT;
            copyStart = i;
          }
        }
        continue;
      }

      if (this.state === OSC8_DROP) {
        if (code === 0x07 || code === 0x9c) {
          this.state = OSC8_TEXT;
          copyStart = i + 1;
        } else if (code === 0x1b) {
          this.state = OSC8_DROP_ESC;
        } else if (++this.dropped >= OSC8_MAX_BODY) {
          this.state = OSC8_TEXT;
          copyStart = i + 1;
        }
        continue;
      }

      // OSC8_DROP_ESC
      if (code === 0x5c || code === 0x07 || code === 0x9c) {
        this.state = OSC8_TEXT;
        copyStart = i + 1;
      } else if (code === 0x1b) {
        if (++this.dropped >= OSC8_MAX_BODY) {
          this.state = OSC8_TEXT;
          copyStart = i + 1;
        }
      } else if (++this.dropped >= OSC8_MAX_BODY) {
        this.state = OSC8_TEXT;
        copyStart = i + 1;
      } else {
        this.state = OSC8_DROP;
      }
    }

    if (copyStart >= 0 && copyStart < chunk.length) {
      out += chunk.slice(copyStart);
    }
    return out;
  }
}

// --------------------------------------------------------------- stream replay

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { ignoreBOM: true });
const statusOrder: Record<SessionStatus["state"], number> = {
  stopped: 0,
  starting: 1,
  running: 2,
  exited: 3,
  failed: 3,
};

/**
 * Reconciles `terminal_snapshot` responses and live `sessionOutput` /
 * `terminalStatus` events using UTF-8 byte offsets.
 *
 * Across a guest disconnect/reconnect (`markDisconnected()`), the daemon may
 * either still hold the same running generation (in which case we seamlessly
 * append only missed bytes) or have restarted with a lower generation number
 * (which is accepted cleanly instead of getting wedged).
 */
export class TerminalReplay {
  generation: number | undefined;
  status: SessionStatus | undefined;
  private end = 0;
  private allowReset = false;

  get endOffset(): number {
    return this.end;
  }

  /**
   * Call when the guest link drops or a fresh host snapshot signals reconnect.
   * Preserves `generation` and `end` so an intact PTY resumes without clearing
   * the screen, while permitting a lower generation if the daemon restarted.
   */
  markDisconnected() {
    this.allowReset = true;
  }

  /** Reset all stream tracking (e.g. before a full reload). */
  clear() {
    this.generation = undefined;
    this.status = undefined;
    this.end = 0;
    this.allowReset = false;
  }

  /** Apply a snapshot. Returns `{ reset, data }` describing what to feed the emulator. */
  snapshot(snapshot: TerminalSnapshot): { reset: boolean; data: string } {
    if (!this.allowReset && this.generation != null && snapshot.generation < this.generation) {
      return { reset: false, data: "" };
    }
    const wasDisconnected = this.allowReset;
    this.allowReset = false;

    const sameGeneration = snapshot.generation === this.generation;
    const reset =
      !sameGeneration ||
      this.end < snapshot.startOffset ||
      (wasDisconnected && snapshot.endOffset < this.end);

    if (reset || !this.status || statusOrder[snapshot.status.state] >= statusOrder[this.status.state]) {
      this.status = snapshot.status;
    }
    this.generation = snapshot.generation;

    if (reset || snapshot.endOffset > this.end) {
      const data = reset
        ? snapshot.data
        : decoder.decode(encoder.encode(snapshot.data).subarray(this.end - snapshot.startOffset));
      this.end = snapshot.endOffset;
      return { reset, data };
    }
    return { reset: false, data: "" };
  }

  /**
   * Apply one live event.
   * Returns `null` when a gap or newer generation requires fetching a snapshot.
   */
  event(event: TerminalEvent): string | null {
    const generation = this.generation;
    if (this.allowReset || generation == null || event.generation > generation) return null;
    if (event.generation < generation) return "";

    if (event.type === "terminalStatus") {
      if (!this.status || statusOrder[event.status.state] >= statusOrder[this.status.state]) {
        this.status = event.status;
      }
      return "";
    }

    if (event.offset > this.end) return null;
    const bytes = encoder.encode(event.data);
    const end = event.offset + bytes.length;
    if (end <= this.end) return "";

    const data =
      event.offset === this.end
        ? event.data
        : decoder.decode(bytes.subarray(this.end - event.offset));
    this.end = end;
    return data;
  }
}

// ---------------------------------------------------------------------- sizing

export interface Grid {
  cols: number;
  rows: number;
}

export const MIN_GRID: Grid = { cols: 20, rows: 5 };
export const PTY_SETTLE_MS = 80;

/**
 * Whole cells that fit in a `width × height` CSS-px box for a cell metric.
 * Returns null while the box or font metrics are not measurable yet.
 */
export function gridFor(width: number, height: number, cell: { width: number; height: number }): Grid | null {
  if (!(width > 0 && height > 0 && cell.width > 0 && cell.height > 0)) return null;
  return {
    cols: Math.max(MIN_GRID.cols, Math.floor(width / cell.width)),
    rows: Math.max(MIN_GRID.rows, Math.floor(height / cell.height)),
  };
}

/**
 * Debounces PTY `SIGWINCH` delivery until sizing settles, while letting the
 * local canvas resize every frame. Call `reset()` after a disconnect so the
 * current size will be re-sent on recovery.
 */
export function settledSender(send: (grid: Grid) => void, delay = PTY_SETTLE_MS) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let latest: Grid | null = null;
  let sent = "";
  const deliver = () => {
    timer = undefined;
    if (!latest) return;
    const key = `${latest.cols}x${latest.rows}`;
    if (key === sent) return;
    sent = key;
    send(latest);
  };
  return {
    push(grid: Grid) {
      latest = grid;
      clearTimeout(timer);
      timer = setTimeout(deliver, delay);
    },
    flush() {
      clearTimeout(timer);
      deliver();
    },
    cancel() {
      clearTimeout(timer);
      timer = undefined;
    },
    reset() {
      clearTimeout(timer);
      timer = undefined;
      sent = "";
    },
  };
}

// ------------------------------------------------------------ viewport anchor

type ReadLine = (index: number) => string | undefined;

export interface ViewportAnchor {
  index: number;
  rows: string[];
}

/** Capture the top visible scrollback line plus context before a write. */
export function captureViewport(length: number, offset: number, line: ReadLine): ViewportAnchor | null {
  if (offset < 1 || length === 0) return null;
  const index = length - Math.min(length, Math.floor(offset));
  const rows: string[] = [];
  for (let row = index; row < Math.min(length, index + 3); row++) {
    const text = line(row);
    if (text === undefined) break;
    rows.push(text);
  }
  return rows.length ? { index, rows } : null;
}

/**
 * Locate the anchored line after new output (including circular scrollback
 * eviction where `length` stops growing). Falls back to the oldest surviving
 * row if the anchor was evicted.
 */
export function restoreViewport(anchor: ViewportAnchor, length: number, line: ReadLine): number {
  for (let index = Math.min(anchor.index, length - anchor.rows.length); index >= 0; index--) {
    if (anchor.rows.every((text, row) => line(index + row) === text)) return length - index;
  }
  return length;
}

// --------------------------------------------------------------- mouse & wheel

export const WHEEL_UP = 64;
export const WHEEL_DOWN = 65;
export const MOUSE_LEFT = 0;
export const MOUSE_MIDDLE = 1;
export const MOUSE_RIGHT = 2;
export const MOUSE_RELEASE = 3;

export interface MouseReportOptions {
  sgr: boolean;
  release?: boolean;
  motion?: boolean;
  shift?: boolean;
  alt?: boolean;
  ctrl?: boolean;
}

/** Encode an xterm mouse report (SGR mode 1006 or legacy X10). */
export function mouseReport(button: number, col: number, row: number, options: MouseReportOptions): string {
  let code = options.release && !options.sgr ? MOUSE_RELEASE : button;
  if (options.shift) code |= 4;
  if (options.alt) code |= 8;
  if (options.ctrl) code |= 16;
  if (options.motion) code |= 32;
  if (options.sgr) {
    const suffix = options.release ? "m" : "M";
    return `\x1b[<${code};${Math.max(1, col)};${Math.max(1, row)}${suffix}`;
  }
  const byte = (value: number) => String.fromCharCode(32 + Math.min(Math.max(value, 1), 223));
  return `\x1b[M${String.fromCharCode(32 + code)}${byte(col)}${byte(row)}`;
}

/** Encode one wheel notch at a 1-based cell. */
export function wheelReport(button: number, col: number, row: number, sgr: boolean): string {
  return mouseReport(button, col, row, { sgr });
}

/** Accumulates fractional trackpad/wheel deltas into whole line notches. */
export class WheelAccumulator {
  private carry = 0;

  take(deltaY: number, deltaMode: number, cellHeight: number, rows: number): number {
    const lines =
      deltaMode === 1
        ? deltaY
        : deltaMode === 2
          ? deltaY * rows
          : deltaY / Math.max(cellHeight, 1);
    if (Math.sign(lines) !== Math.sign(this.carry)) this.carry = 0;
    this.carry += lines;
    const whole = Math.trunc(this.carry);
    this.carry -= whole;
    return Math.max(-10, Math.min(10, whole));
  }

  reset() {
    this.carry = 0;
  }
}

/** 1-based cell under `(x, y)` clamped to the grid. */
export function cellAt(
  x: number,
  y: number,
  rect: { left: number; top: number; width: number; height: number },
  cols: number,
  rows: number,
): { col: number; row: number } {
  const col = Math.floor(((x - rect.left) / Math.max(rect.width, 1)) * cols) + 1;
  const row = Math.floor(((y - rect.top) / Math.max(rect.height, 1)) * rows) + 1;
  return {
    col: Math.min(Math.max(col, 1), cols),
    row: Math.min(Math.max(row, 1), rows),
  };
}

// ------------------------------------------------------------ keyboard encoder

export interface KeyEventLike {
  key: string;
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

export interface KeyEncodeOptions {
  appCursor: boolean;
  isMac: boolean;
  altScreen: boolean;
}

function modifierParam(event: KeyEventLike): number {
  return 1 + (event.shiftKey ? 1 : 0) + (event.altKey ? 2 : 0) + (event.ctrlKey ? 4 : 0);
}

const ARROW_FINAL: Record<string, string> = {
  ArrowUp: "A",
  ArrowDown: "B",
  ArrowRight: "C",
  ArrowLeft: "D",
  Home: "H",
  End: "F",
};

const TILDE_KEY: Record<string, number> = {
  Insert: 2,
  Delete: 3,
  PageUp: 5,
  PageDown: 6,
  F5: 15,
  F6: 17,
  F7: 18,
  F8: 19,
  F9: 20,
  F10: 21,
  F11: 23,
  F12: 24,
};

const F1_F4: Record<string, string> = {
  F1: "P",
  F2: "Q",
  F3: "R",
  F4: "S",
};

/**
 * Encode a browser `keydown` into an xterm-256color byte sequence.
 * Returns `null` when the key should not be sent (pure modifiers, host shortcuts).
 */
export function encodeKeyEvent(event: KeyEventLike, options: KeyEncodeOptions): string | null {
  const { key, code, ctrlKey, altKey, shiftKey, metaKey } = event;
  if (key === "Shift" || key === "Control" || key === "Alt" || key === "Meta" || key === "CapsLock" || key === "Dead") {
    return null;
  }

  // macOS line-editing shortcuts on the normal prompt.
  if (options.isMac && metaKey && !ctrlKey && !altKey && !shiftKey) {
    if (!options.altScreen) {
      if (key === "Backspace") return "\x15";
      if (key === "ArrowLeft") return "\x01";
      if (key === "ArrowRight") return "\x05";
    }
    return null;
  }
  if (metaKey) return null;

  // Tab & Shift+Tab (essential for Claude Code, Codex, OpenCode, vim, fzf).
  if (key === "Tab") {
    return shiftKey ? "\x1b[Z" : "\t";
  }

  // Enter / Alt+Enter.
  if (key === "Enter") {
    return altKey ? "\x1b\r" : "\r";
  }

  // Backspace.
  if (key === "Backspace") {
    if (altKey) return "\x1b\x7f";
    if (ctrlKey) return "\x08";
    return "\x7f";
  }

  // Escape.
  if (key === "Escape") {
    return altKey ? "\x1b\x1b" : "\x1b";
  }

  // Arrow keys, Home, End.
  const arrow = ARROW_FINAL[key];
  if (arrow) {
    const mod = modifierParam(event);
    if (mod === 1) {
      return options.appCursor ? `\x1bO${arrow}` : `\x1b[${arrow}`;
    }
    // Option+Left/Right on macOS in normal shells: standard Meta+b / Meta+f word motion.
    if (options.isMac && !options.altScreen && altKey && !ctrlKey && !shiftKey) {
      if (key === "ArrowLeft") return "\x1bb";
      if (key === "ArrowRight") return "\x1bf";
    }
    return `\x1b[1;${mod}${arrow}`;
  }

  // F1–F4.
  const fn14 = F1_F4[key];
  if (fn14) {
    const mod = modifierParam(event);
    return mod === 1 ? `\x1bO${fn14}` : `\x1b[1;${mod}${fn14}`;
  }

  // Insert, Delete, PageUp, PageDown, F5–F12.
  const tilde = TILDE_KEY[key];
  if (tilde !== undefined) {
    const mod = modifierParam(event);
    return mod === 1 ? `\x1b[${tilde}~` : `\x1b[${tilde};${mod}~`;
  }

  // AltGr on Windows/Linux (ctrlKey && altKey producing a printable character).
  if (ctrlKey && altKey && key.length === 1) {
    return key;
  }

  // Ctrl combinations.
  if (ctrlKey && !altKey) {
    if (key === " " || code === "Space" || key === "@") return "\x00";
    if (key.length === 1) {
      const lower = key.toLowerCase();
      const cp = lower.charCodeAt(0);
      if (cp >= 0x61 && cp <= 0x7a) return String.fromCharCode(cp - 0x60);
      if (key === "[") return "\x1b";
      if (key === "\\") return "\x1c";
      if (key === "]") return "\x1d";
      if (key === "^" || key === "6") return "\x1e";
      if (key === "_" || key === "-" || key === "/") return "\x1f";
    }
    // Fallback to physical key code for non-Latin keyboard layouts with Ctrl.
    if (code.startsWith("Key") && code.length === 4) {
      const cp = code.charCodeAt(3);
      if (cp >= 0x41 && cp <= 0x5a) return String.fromCharCode(cp - 0x40);
    }
    return null;
  }

  // Alt / Option combinations.
  if (altKey && !ctrlKey) {
    if (key.length === 1) {
      // Non-ASCII character composed by Option (e.g. non-US keyboards): send directly.
      if (options.isMac && key.charCodeAt(0) > 127) return key;
      return `\x1b${key}`;
    }
    return null;
  }

  // Plain printable character.
  if (key.length === 1) {
    return key;
  }

  return null;
}

// ----------------------------------------------------------------------- paste

export const PASTE_IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"] as const;
export const PASTE_IMAGE_MAX = 20 * 1024 * 1024;

interface ClipboardItemLike {
  kind: string;
  type: string;
  getAsFile(): Blob | null;
}

/** First supported image in a paste, or null (then treat as text). */
export function pastedImage(items: ArrayLike<ClipboardItemLike> | null | undefined): Blob | null {
  for (const item of Array.from(items ?? [])) {
    if (item.kind === "file" && (PASTE_IMAGE_TYPES as readonly string[]).includes(item.type)) {
      const file = item.getAsFile();
      if (file) return file;
    }
  }
  return null;
}

/** Quote a guest path only when a shell would split or interpret it. */
export function typedPath(path: string): string {
  return /^[\w@%+=:,./-]+$/.test(path) ? path : `'${path.replace(/'/g, `'\\''`)}'`;
}

/** Wrap text with bracketed paste sequences (DEC mode 2004) when enabled. */
export function wrapBracketedPaste(text: string, bracketed: boolean): string {
  // Strip any embedded bracketed-paste end markers so untrusted clipboard text cannot escape.
  const safe = bracketed ? text.split("\x1b[201~").join("") : text;
  return bracketed ? `\x1b[200~${safe}\x1b[201~` : text;
}

/** Encode a Blob to plain base64 without a data-URL prefix. */
export async function blobBase64(blob: Blob): Promise<string> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}
