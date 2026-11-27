/**
 * Self-contained desktop terminal engine built directly on libghostty WASM
 * (`Ghostty` + `GhosttyTerminal`), replacing ghostty-web's fragile JS wrapper.
 *
 * Responsibilities:
 * - WASM VT lifecycle (always RIS-reset so recycled WASM memory stays clean)
 * - Demand-driven Canvas 2D rendering (zero idle CPU, zero background rAF)
 * - Exact container fitting, DPR tracking, and instant offline theme/font updates
 * - Clean `<textarea>` keyboard & IME composition (no `contenteditable` hacks)
 * - Full selection (single/double/triple click, auto-scroll, soft-wrap copy)
 * - Scrollback search (`Cmd+F` / `Ctrl+Shift+F`) and Cmd/Ctrl-click URLs
 * - Full SGR/X10 mouse + wheel reporting with Shift local-override
 */
import { Ghostty, type GhosttyTerminal } from "ghostty-web";
import {
  extractSelectionText, findLineBounds, findUrlAt, findWordBounds, normalizeRange,
  searchTerminalBuffer, type BufferPos, type BufferRange, type SearchMatch, type UrlMatch,
} from "./buffer";
import {
  cellAt, encodeKeyEvent, gridFor, MOUSE_LEFT, MOUSE_MIDDLE, MOUSE_RIGHT, mouseReport,
  pastedImage, captureViewport, restoreViewport, WheelAccumulator, wheelReport,
  WHEEL_DOWN, WHEEL_UP, wrapBracketedPaste, type Grid,
} from "./protocol";
import { TerminalCanvasRenderer } from "./renderer";
import {
  type ColorScheme, TERMINAL_FONT, TERMINAL_SCROLLBACK_LIMIT, terminalColors,
  toWasmTerminalConfig, type TerminalColors,
} from "./theme";
import { IS_MAC } from "../platform";

const RAW_HISTORY_LIMIT = 512 * 1024;
const RAW_HISTORY_HEADROOM = 64 * 1024;

let ghosttyPromise: Promise<Ghostty> | undefined;

/** Load the Ghostty WASM VT engine once, allowing retries on failure. */
export function loadGhostty(): Promise<Ghostty> {
  ghosttyPromise ??= Ghostty.load().catch((error: unknown) => {
    ghosttyPromise = undefined;
    throw error;
  });
  return ghosttyPromise;
}

export interface ScrollState {
  viewportY: number;
  scrollbackLength: number;
  rows: number;
  isAltScreen: boolean;
  hasUnreadBelow: boolean;
}

export interface SearchState {
  query: string;
  caseSensitive: boolean;
  total: number;
  activeIndex: number;
}

export interface ContextMenuRequest {
  x: number;
  y: number;
  hasSelection: boolean;
}

export interface TerminalEngineCallbacks {
  onData?: (data: string) => void;
  onResize?: (grid: Grid) => void;
  onScroll?: (scroll: ScrollState) => void;
  onZoom?: (delta: number) => void;
  onSearchOpen?: () => void;
  onSearchUpdate?: (state: SearchState) => void;
  onCopy?: (text: string) => void;
  onPasteRequest?: () => Promise<string | null>;
  onPasteImage?: (image: Blob) => void;
  onOpenUrl?: (url: string) => void;
  onContextMenu?: (request: ContextMenuRequest) => void;
}

function appendBoundedHistory(current: string, chunk: string): string {
  const combined = current + chunk;
  if (combined.length <= RAW_HISTORY_LIMIT) return combined;
  let cut = combined.length - RAW_HISTORY_LIMIT + RAW_HISTORY_HEADROOM;
  const nl = combined.indexOf("\n", cut);
  if (nl !== -1 && nl < cut + 8192) cut = nl + 1;
  return combined.slice(cut);
}

export class TerminalEngine {
  readonly root: HTMLDivElement;
  readonly canvas: HTMLCanvasElement;
  readonly textarea: HTMLTextAreaElement;

  private readonly ghostty: Ghostty;
  private wasmTerm: GhosttyTerminal;
  private readonly renderer: TerminalCanvasRenderer;
  private scheme: ColorScheme;
  private colors: TerminalColors;
  private fontSize: number;
  private grid: Grid;
  private callbacks: TerminalEngineCallbacks;

  private mounted = false;
  private disposed = false;
  private focused = false;
  private viewportY = 0;
  private hasUnreadBelow = false;
  private rawHistory = "";

  // Rendering & observation
  private renderFrame = 0;
  private forceFullRender = true;
  private resizeObserver: ResizeObserver | null = null;
  private fitFrame = 0;
  private dprMedia: MediaQueryList | null = null;
  private slot: HTMLElement | null = null;

  // Selection & mouse state
  private selection: BufferRange | null = null;
  private anchorPos: BufferPos | null = null;
  private selecting = false;
  private mousePressedButton: number | null = null;
  private autoScrollTimer: ReturnType<typeof setInterval> | null = null;
  private autoScrollDir = 0;
  private hoveredUrl: UrlMatch | null = null;
  private modifierHeld = false;

  // Search state
  private searchQuery = "";
  private searchCaseSensitive = false;
  private searchMatches: SearchMatch[] = [];
  private activeMatchIndex = -1;
  private searchRefreshTimer: ReturnType<typeof setTimeout> | undefined;

  // Input / IME / wheel state
  private readonly remoteWheel = new WheelAccumulator();
  private readonly localWheel = new WheelAccumulator();
  private composing = false;
  private preedit = "";
  private lastCommit = "";
  private commitTimer: ReturnType<typeof setTimeout> | undefined;
  private readonly measureCtx: CanvasRenderingContext2D | null;

  // Bound listeners cleaned up on dispose
  private readonly cleanups: (() => void)[] = [];

  constructor(
    ghostty: Ghostty,
    options: {
      scheme: ColorScheme;
      fontSize: number;
      cols?: number;
      rows?: number;
      callbacks?: TerminalEngineCallbacks;
    },
  ) {
    this.ghostty = ghostty;
    this.scheme = options.scheme;
    this.colors = terminalColors(options.scheme);
    this.fontSize = options.fontSize;
    this.grid = { cols: options.cols ?? 80, rows: options.rows ?? 24 };
    this.callbacks = options.callbacks ?? {};

    this.root = document.createElement("div");
    this.root.className = "terminal-host";

    this.canvas = document.createElement("canvas");
    this.canvas.className = "terminal-canvas";
    this.root.appendChild(this.canvas);

    this.textarea = document.createElement("textarea");
    this.textarea.className = "terminal-ime";
    this.textarea.setAttribute("autocorrect", "off");
    this.textarea.setAttribute("autocapitalize", "off");
    this.textarea.setAttribute("autocomplete", "off");
    this.textarea.setAttribute("spellcheck", "false");
    this.textarea.setAttribute("aria-label", "Terminal input");
    this.textarea.wrap = "off";
    this.root.appendChild(this.textarea);

    this.measureCtx = document.createElement("canvas").getContext("2d");
    this.wasmTerm = this.createWasmTerminal(this.grid.cols, this.grid.rows);
    this.renderer = new TerminalCanvasRenderer(this.canvas, {
      fontSize: this.fontSize,
      fontFamily: TERMINAL_FONT,
      colors: this.colors,
    });

    this.bindEvents();
  }

  private createWasmTerminal(cols: number, rows: number): GhosttyTerminal {
    const term = this.ghostty.createTerminal(
      cols,
      rows,
      toWasmTerminalConfig(this.colors, TERMINAL_SCROLLBACK_LIMIT),
    );
    // Always perform RIS so recycled WASM heap pages never leak prior cells or modes.
    term.write("\x1bc");
    while (term.readResponse() !== null) {
      // Discard any reset responses.
    }
    return term;
  }

  // ------------------------------------------------------------------ getters

  getGrid(): Grid {
    return { ...this.grid };
  }

  getScheme(): ColorScheme {
    return this.scheme;
  }

  getFontSize(): number {
    return this.fontSize;
  }

  getScrollState(): ScrollState {
    return {
      viewportY: this.viewportY,
      scrollbackLength: this.wasmTerm.getScrollbackLength(),
      rows: this.grid.rows,
      isAltScreen: this.wasmTerm.isAlternateScreen(),
      hasUnreadBelow: this.hasUnreadBelow,
    };
  }

  hasSelection(): boolean {
    return this.selection !== null;
  }

  getSelection(): string {
    if (!this.selection) return "";
    return extractSelectionText(this.wasmTerm, this.selection.start, this.selection.end);
  }

  isFocused(): boolean {
    return this.focused;
  }

  setCallbacks(callbacks: TerminalEngineCallbacks) {
    this.callbacks = callbacks;
  }

  // ---------------------------------------------------------------- lifecycle

  mount(slot: HTMLElement) {
    if (this.disposed) return;
    this.unmount();
    this.slot = slot;
    this.mounted = true;
    if (this.root.parentElement !== slot) slot.appendChild(this.root);

    this.renderer.remeasure();
    this.fit(true);

    this.resizeObserver = new ResizeObserver(() => {
      cancelAnimationFrame(this.fitFrame);
      this.fitFrame = requestAnimationFrame(() => this.fit(false));
    });
    this.resizeObserver.observe(slot);

    this.watchDpr();
    this.positionIme();
    this.scheduleRender(true);
    this.emitScroll();
  }

  unmount() {
    this.mounted = false;
    this.slot = null;
    this.stopAutoScroll();
    cancelAnimationFrame(this.renderFrame);
    this.renderFrame = 0;
    cancelAnimationFrame(this.fitFrame);
    this.fitFrame = 0;
    this.resizeObserver?.disconnect();
    this.resizeObserver = null;
    this.unwatchDpr();
    this.root.remove();
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.unmount();
    clearTimeout(this.commitTimer);
    clearTimeout(this.searchRefreshTimer);
    for (const off of this.cleanups) off();
    this.cleanups.length = 0;
    this.wasmTerm.free();
  }

  private watchDpr() {
    this.unwatchDpr();
    if (typeof window === "undefined" || !window.matchMedia) return;
    const onDprChange = () => {
      this.watchDpr();
      this.renderer.remeasure();
      this.fit(false);
      this.scheduleRender(true);
    };
    this.dprMedia = window.matchMedia(`(resolution: ${window.devicePixelRatio || 1}dppx)`);
    this.dprMedia.addEventListener("change", onDprChange, { once: true });
  }

  private unwatchDpr() {
    this.dprMedia = null;
  }

  // ------------------------------------------------------------ focus & sizing

  focus() {
    if (this.disposed || !this.mounted) return;
    this.positionIme();
    if (document.activeElement !== this.textarea) {
      this.textarea.focus({ preventScroll: true });
    }
  }

  blur() {
    if (document.activeElement === this.textarea) this.textarea.blur();
  }

  /** Measure the mounted slot and resize the WASM grid if whole-cell dims changed. */
  fit(immediate = false): Grid | null {
    if (this.disposed || !this.slot) return null;
    const rect = this.slot.getBoundingClientRect();
    const width = Math.floor(rect.width);
    const height = Math.floor(rect.height);
    const next = gridFor(width, height, this.renderer.getMetrics());
    if (!next) return null;

    if (next.cols !== this.grid.cols || next.rows !== this.grid.rows) {
      this.grid = next;
      this.wasmTerm.resize(next.cols, next.rows);
      this.viewportY = Math.min(this.viewportY, this.wasmTerm.getScrollbackLength());
      this.selection = null;
      this.hoveredUrl = null;
      this.positionIme();
      this.scheduleRender(true);
      this.emitScroll();
      this.callbacks.onResize?.(next);
      if (this.searchQuery) this.scheduleSearchRefresh();
    } else if (immediate) {
      this.scheduleRender(true);
      this.callbacks.onResize?.(next);
    }
    return next;
  }

  setFontSize(size: number) {
    if (this.disposed || size === this.fontSize) return;
    this.fontSize = size;
    this.renderer.setFont(size, TERMINAL_FONT);
    this.selection = null;
    this.hoveredUrl = null;
    if (this.mounted) {
      this.fit(true);
      this.positionIme();
      this.scheduleRender(true);
    }
  }

  /** Switch light/dark theme in place without dropping history or scroll position. */
  setTheme(scheme: ColorScheme) {
    if (this.disposed || scheme === this.scheme) return;
    this.scheme = scheme;
    this.colors = terminalColors(scheme);
    this.renderer.setColors(this.colors);

    const prevOffset = this.viewportY;
    const prevTerm = this.wasmTerm;
    const nextTerm = this.createWasmTerminal(this.grid.cols, this.grid.rows);
    if (this.rawHistory) {
      nextTerm.write(this.rawHistory);
      while (nextTerm.readResponse() !== null) {
        // Suppress responses during local theme replay.
      }
    }
    this.wasmTerm = nextTerm;
    prevTerm.free();

    this.viewportY = Math.min(prevOffset, this.wasmTerm.getScrollbackLength());
    this.selection = null;
    this.scheduleRender(true);
    this.emitScroll();
    if (this.searchQuery) this.runSearch();
  }

  // ------------------------------------------------------------------ writing

  write(
    data: string,
    options: { suppressResponses?: boolean; reset?: boolean; preserveViewport?: boolean } = {},
  ) {
    if (this.disposed) return;
    const { suppressResponses = false, reset = false, preserveViewport = true } = options;

    const beforeLen = this.wasmTerm.getScrollbackLength();
    const wasScrolledUp = this.viewportY > 0;
    const lineReader = (index: number) => {
      const cells = this.wasmTerm.getScrollbackLine(index);
      if (!cells) return undefined;
      return cells
        .filter((cell) => cell.width !== 0)
        .map((cell) => String.fromCodePoint(cell.codepoint || 32))
        .join("")
        .trimEnd();
    };
    const anchor =
      preserveViewport && !reset && wasScrolledUp && !this.wasmTerm.isAlternateScreen()
        ? captureViewport(beforeLen, this.viewportY, lineReader)
        : null;

    if (reset) {
      this.wasmTerm.write("\x1bc");
      while (this.wasmTerm.readResponse() !== null) {
        // Clear reset responses.
      }
      this.rawHistory = "";
      this.selection = null;
      this.hoveredUrl = null;
      this.hasUnreadBelow = false;
      if (!preserveViewport) this.viewportY = 0;
    }

    if (data) {
      this.wasmTerm.write(data);
      this.rawHistory = appendBoundedHistory(this.rawHistory, data);
    }

    // Drain terminal responses (DSR cursor position, DA, etc.).
    let reply: string | null;
    while ((reply = this.wasmTerm.readResponse()) !== null) {
      if (!suppressResponses && reply) {
        this.callbacks.onData?.(reply);
      }
    }

    const afterLen = this.wasmTerm.getScrollbackLength();
    if (this.wasmTerm.isAlternateScreen()) {
      this.viewportY = 0;
      this.hasUnreadBelow = false;
    } else if (wasScrolledUp && preserveViewport) {
      if (anchor) {
        this.viewportY = restoreViewport(anchor, afterLen, lineReader);
      } else {
        this.viewportY = Math.min(afterLen, this.viewportY);
      }
      if (data && !reset && this.viewportY > 0) {
        this.hasUnreadBelow = true;
      }
    } else if (reset && preserveViewport) {
      this.viewportY = Math.min(afterLen, this.viewportY);
    } else {
      this.viewportY = 0;
      this.hasUnreadBelow = false;
    }

    this.positionIme();
    this.scheduleRender(reset || this.viewportY > 0);
    this.emitScroll();
    if (this.searchQuery) this.scheduleSearchRefresh();
  }

  /** Clear screen and local scrollback (like Cmd+K in native terminals). */
  clearLocal() {
    if (this.disposed) return;
    this.wasmTerm.write("\x1b[2J\x1b[3J\x1b[H");
    while (this.wasmTerm.readResponse() !== null) {
      // Ignore responses.
    }
    this.rawHistory = "";
    this.viewportY = 0;
    this.hasUnreadBelow = false;
    this.selection = null;
    this.hoveredUrl = null;
    this.scheduleRender(true);
    this.emitScroll();
    if (this.searchQuery) this.runSearch();
  }

  // ---------------------------------------------------------------- scrolling

  scrollTo(viewportY: number) {
    if (this.disposed) return;
    if (this.wasmTerm.isAlternateScreen()) {
      if (this.viewportY !== 0) {
        this.viewportY = 0;
        this.hasUnreadBelow = false;
        this.emitScroll();
        this.scheduleRender(true);
      }
      return;
    }
    const max = this.wasmTerm.getScrollbackLength();
    const clamped = Math.max(0, Math.min(max, Math.round(viewportY)));
    if (clamped === this.viewportY) return;
    this.viewportY = clamped;
    if (clamped === 0) this.hasUnreadBelow = false;
    this.hoveredUrl = null;
    this.positionIme();
    this.scheduleRender(true);
    this.emitScroll();
  }

  /** Scroll by signed lines (negative = older history, positive = toward bottom). */
  scrollBy(deltaLines: number) {
    this.scrollTo(this.viewportY - deltaLines);
  }

  scrollToBottom() {
    this.scrollTo(0);
  }

  scrollToTop() {
    this.scrollTo(this.wasmTerm.getScrollbackLength());
  }

  private emitScroll() {
    this.callbacks.onScroll?.(this.getScrollState());
  }

  // ---------------------------------------------------------------- selection

  selectAll() {
    if (this.disposed) return;
    const total = this.wasmTerm.getScrollbackLength() + this.grid.rows;
    if (total <= 0) return;
    this.selection = {
      start: { absRow: 0, col: 0 },
      end: { absRow: total - 1, col: Math.max(0, this.grid.cols - 1) },
    };
    this.scheduleRender(true);
  }

  clearSelection() {
    if (!this.selection) return;
    this.selection = null;
    this.scheduleRender(true);
  }

  copySelection(): boolean {
    const text = this.getSelection();
    if (!text) return false;
    this.callbacks.onCopy?.(text);
    return true;
  }

  pasteText(text: string) {
    if (this.disposed || !text) return;
    this.scrollToBottom();
    const payload = wrapBracketedPaste(text, this.wasmTerm.hasBracketedPaste());
    this.callbacks.onData?.(payload);
  }

  // ------------------------------------------------------------------- search

  setSearch(query: string, caseSensitive = this.searchCaseSensitive) {
    this.searchQuery = query;
    this.searchCaseSensitive = caseSensitive;
    this.runSearch(true);
  }

  clearSearch() {
    clearTimeout(this.searchRefreshTimer);
    this.searchQuery = "";
    this.searchMatches = [];
    this.activeMatchIndex = -1;
    this.scheduleRender(true);
    this.emitSearch();
  }

  nextMatch() {
    if (this.searchMatches.length === 0) return;
    this.activeMatchIndex = (this.activeMatchIndex + 1) % this.searchMatches.length;
    this.scrollMatchIntoView(this.searchMatches[this.activeMatchIndex]);
    this.scheduleRender(true);
    this.emitSearch();
  }

  prevMatch() {
    if (this.searchMatches.length === 0) return;
    this.activeMatchIndex =
      (this.activeMatchIndex - 1 + this.searchMatches.length) % this.searchMatches.length;
    this.scrollMatchIntoView(this.searchMatches[this.activeMatchIndex]);
    this.scheduleRender(true);
    this.emitSearch();
  }

  private scheduleSearchRefresh() {
    clearTimeout(this.searchRefreshTimer);
    this.searchRefreshTimer = setTimeout(() => this.runSearch(false), 120);
  }

  private runSearch(jumpToNearest = false) {
    if (!this.searchQuery) {
      this.searchMatches = [];
      this.activeMatchIndex = -1;
      this.scheduleRender(true);
      this.emitSearch();
      return;
    }
    this.searchMatches = searchTerminalBuffer(this.wasmTerm, this.searchQuery, {
      caseSensitive: this.searchCaseSensitive,
    });
    if (this.searchMatches.length === 0) {
      this.activeMatchIndex = -1;
    } else if (jumpToNearest || this.activeMatchIndex < 0 || this.activeMatchIndex >= this.searchMatches.length) {
      // Jump to the most recent match (nearest the bottom).
      this.activeMatchIndex = this.searchMatches.length - 1;
      if (jumpToNearest) this.scrollMatchIntoView(this.searchMatches[this.activeMatchIndex]);
    }
    this.scheduleRender(true);
    this.emitSearch();
  }

  private scrollMatchIntoView(match: SearchMatch) {
    if (this.wasmTerm.isAlternateScreen()) return;
    const scrollback = this.wasmTerm.getScrollbackLength();
    const topAbs = scrollback - this.viewportY;
    const bottomAbs = topAbs + this.grid.rows - 1;
    if (match.absRow >= topAbs && match.absRow <= bottomAbs) return;
    const targetTop = Math.max(0, match.absRow - Math.floor(this.grid.rows / 3));
    this.scrollTo(scrollback - targetTop);
  }

  private emitSearch() {
    this.callbacks.onSearchUpdate?.({
      query: this.searchQuery,
      caseSensitive: this.searchCaseSensitive,
      total: this.searchMatches.length,
      activeIndex: this.activeMatchIndex,
    });
  }

  // ---------------------------------------------------------------- rendering

  private scheduleRender(forceFull = false) {
    if (this.disposed || !this.mounted) return;
    if (forceFull) this.forceFullRender = true;
    if (this.renderFrame) return;
    this.renderFrame = requestAnimationFrame(() => {
      this.renderFrame = 0;
      if (this.disposed || !this.mounted) return;
      const full = this.forceFullRender;
      this.forceFullRender = false;
      this.renderer.render(
        this.wasmTerm,
        {
          viewportY: this.viewportY,
          focused: this.focused,
          selection: this.selection,
          matches: this.searchMatches,
          activeMatchIndex: this.activeMatchIndex,
          hoveredUrl: this.modifierHeld ? this.hoveredUrl : null,
        },
        full,
      );
    });
  }

  // ---------------------------------------------------------------------- IME

  private positionIme() {
    if (this.disposed) return;
    const cursor = this.wasmTerm.getCursor();
    const metrics = this.renderer.getMetrics();
    const col = Math.max(0, Math.min(this.grid.cols - 1, cursor.x));
    const visRow = cursor.y + this.viewportY;
    const row = Math.max(0, Math.min(this.grid.rows - 1, visRow));
    const font = `400 ${this.fontSize}px ${TERMINAL_FONT}`;
    if (this.measureCtx) this.measureCtx.font = font;
    const preeditWidth = this.composing
      ? (this.measureCtx?.measureText(this.preedit).width ?? this.preedit.length * metrics.width)
      : 0;
    const width = Math.min(
      Math.max(metrics.width, (this.grid.cols - col) * metrics.width),
      Math.max(metrics.width, Math.ceil(preeditWidth + metrics.width)),
    );

    this.textarea.style.left = `${col * metrics.width}px`;
    this.textarea.style.top = `${row * metrics.height}px`;
    this.textarea.style.width = `${width}px`;
    this.textarea.style.height = `${metrics.height}px`;
    this.textarea.style.font = font;
    this.textarea.style.lineHeight = `${metrics.height}px`;
  }

  // ------------------------------------------------------------------- events

  private listen<K extends keyof HTMLElementEventMap>(
    target: HTMLElement,
    type: K,
    handler: (event: HTMLElementEventMap[K]) => void,
    options?: AddEventListenerOptions,
  ): void;
  private listen<K extends keyof DocumentEventMap>(
    target: Document,
    type: K,
    handler: (event: DocumentEventMap[K]) => void,
    options?: AddEventListenerOptions,
  ): void;
  private listen<K extends keyof WindowEventMap>(
    target: Window,
    type: K,
    handler: (event: WindowEventMap[K]) => void,
    options?: AddEventListenerOptions,
  ): void;
  private listen(
    target: EventTarget,
    type: string,
    handler: EventListener,
    options?: AddEventListenerOptions,
  ) {
    target.addEventListener(type, handler, options);
    this.cleanups.push(() => target.removeEventListener(type, handler, options));
  }

  private pointerToBufferPos(clientX: number, clientY: number): BufferPos {
    const rect = this.canvas.getBoundingClientRect();
    const metrics = this.renderer.getMetrics();
    const col = Math.max(0, Math.min(this.grid.cols - 1, Math.floor((clientX - rect.left) / metrics.width)));
    const visRow = Math.max(0, Math.min(this.grid.rows - 1, Math.floor((clientY - rect.top) / metrics.height)));
    const absRow = this.wasmTerm.getScrollbackLength() - this.viewportY + visRow;
    return { absRow: Math.max(0, absRow), col };
  }

  private pointerToCell(clientX: number, clientY: number) {
    const rect = this.canvas.getBoundingClientRect();
    return cellAt(clientX, clientY, rect, this.grid.cols, this.grid.rows);
  }

  private isMouseTrackingActive(event: { shiftKey: boolean }): boolean {
    return !event.shiftKey && this.wasmTerm.hasMouseTracking();
  }

  private stopAutoScroll() {
    if (this.autoScrollTimer !== null) {
      clearInterval(this.autoScrollTimer);
      this.autoScrollTimer = null;
    }
    this.autoScrollDir = 0;
  }

  private startAutoScroll(dir: number) {
    if (this.autoScrollTimer !== null && this.autoScrollDir === dir) return;
    this.stopAutoScroll();
    this.autoScrollDir = dir;
    this.autoScrollTimer = setInterval(() => {
      if (!this.selecting || !this.anchorPos) {
        this.stopAutoScroll();
        return;
      }
      this.scrollBy(dir * 2);
      const scrollback = this.wasmTerm.getScrollbackLength();
      const targetRow = dir < 0 ? scrollback - this.viewportY : scrollback - this.viewportY + this.grid.rows - 1;
      const targetCol = dir < 0 ? 0 : this.grid.cols - 1;
      this.selection = normalizeRange(this.anchorPos, { absRow: Math.max(0, targetRow), col: targetCol });
      this.scheduleRender(true);
    }, 45);
  }

  private bindEvents() {
    // Focus / blur on textarea.
    this.listen(this.textarea, "focus", () => {
      this.focused = true;
      this.positionIme();
      this.scheduleRender(false);
      if (this.wasmTerm.hasFocusEvents()) this.callbacks.onData?.("\x1b[I");
    });
    this.listen(this.textarea, "blur", () => {
      this.focused = false;
      this.scheduleRender(false);
      if (this.wasmTerm.hasFocusEvents()) this.callbacks.onData?.("\x1b[O");
    });

    // IME composition lifecycle.
    this.listen(this.textarea, "compositionstart", () => {
      this.composing = true;
      this.preedit = "";
      this.lastCommit = "";
      clearTimeout(this.commitTimer);
      this.textarea.classList.add("terminal-composing");
      this.positionIme();
    });
    this.listen(this.textarea, "compositionupdate", (event) => {
      this.preedit = event.data ?? "";
      this.positionIme();
    });
    this.listen(this.textarea, "compositionend", (event) => {
      this.composing = false;
      this.preedit = "";
      const committed = event.data ?? "";
      this.lastCommit = committed;
      this.textarea.classList.remove("terminal-composing");
      this.textarea.value = "";
      if (committed) {
        this.scrollToBottom();
        this.callbacks.onData?.(committed);
      }
      this.positionIme();
      clearTimeout(this.commitTimer);
      this.commitTimer = setTimeout(() => { this.lastCommit = ""; }, 0);
    });
    this.listen(this.textarea, "input", (event) => {
      const input = event as InputEvent;
      if (this.composing || input.isComposing) return;
      if (input.inputType === "insertText" || input.inputType === "insertReplacementText") {
        const text = input.data ?? this.textarea.value;
        if (text && text !== this.lastCommit) {
          this.scrollToBottom();
          this.callbacks.onData?.(text);
        }
      }
      this.lastCommit = "";
      this.textarea.value = "";
    });

    // Keyboard input & shortcuts.
    this.listen(this.textarea, "keydown", (event) => {
      if (this.disposed || this.composing || event.isComposing || event.keyCode === 229) return;

      const primary = IS_MAC ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;

      // Zoom: Cmd/Ctrl + '+', '=', '-', '0' (including Numpad).
      if (primary && !event.altKey && this.callbacks.onZoom) {
        let zoom: number | null = null;
        if (event.key === "0" || event.code === "Digit0" || event.code === "Numpad0") zoom = 0;
        else if (event.key === "-" || event.key === "_" || event.code === "Minus" || event.code === "NumpadSubtract") zoom = -1;
        else if (event.key === "+" || event.key === "=" || event.code === "Equal" || event.code === "NumpadAdd") zoom = 1;
        if (zoom !== null) {
          event.preventDefault();
          event.stopPropagation();
          this.callbacks.onZoom(zoom);
          return;
        }
      }

      // Search: Cmd+F (macOS) or Ctrl+Shift+F (everywhere).
      if ((IS_MAC && event.metaKey && !event.ctrlKey && !event.altKey && event.code === "KeyF") ||
          (event.ctrlKey && event.shiftKey && !event.altKey && !event.metaKey && event.code === "KeyF")) {
        event.preventDefault();
        event.stopPropagation();
        this.callbacks.onSearchOpen?.();
        return;
      }

      // Select all: Cmd+A (macOS) or Ctrl+Shift+A (Win/Linux).
      if ((IS_MAC && event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey && event.code === "KeyA") ||
          (!IS_MAC && event.ctrlKey && event.shiftKey && !event.altKey && !event.metaKey && event.code === "KeyA")) {
        event.preventDefault();
        this.selectAll();
        return;
      }

      // Copy: Cmd+C (macOS) or Ctrl+Shift+C (Win/Linux).
      if ((IS_MAC && event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey && event.code === "KeyC") ||
          (event.ctrlKey && event.shiftKey && !event.altKey && !event.metaKey && event.code === "KeyC")) {
        if (this.hasSelection()) {
          event.preventDefault();
          this.copySelection();
          return;
        }
        if (IS_MAC || event.shiftKey) {
          event.preventDefault();
          return;
        }
      }

      // Paste: Ctrl+Shift+V (explicit clipboard read); plain Cmd+V uses native `paste` event.
      if (event.ctrlKey && event.shiftKey && !event.altKey && !event.metaKey && event.code === "KeyV") {
        event.preventDefault();
        void this.callbacks.onPasteRequest?.().then((text) => {
          if (text) this.pasteText(text);
        });
        return;
      }

      // Allow native paste event on Cmd+V (macOS) to flow to `onPaste`.
      if (IS_MAC && event.metaKey && !event.ctrlKey && !event.altKey && event.code === "KeyV") {
        return;
      }

      // Let desktop app shortcuts bubble to window without hitting the PTY.
      if (event.ctrlKey && event.shiftKey && !event.altKey && ["KeyN", "KeyK", "KeyB"].includes(event.code)) {
        return;
      }
      if (IS_MAC && event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey &&
          ["KeyN", "KeyK", "Comma", "BracketLeft", "BracketRight"].includes(event.code)) {
        return;
      }

      // Local scrollback navigation via Shift+PageUp/PageDown/Home/End.
      if (event.shiftKey && !event.ctrlKey && !event.altKey && !event.metaKey && !this.wasmTerm.isAlternateScreen()) {
        if (event.key === "PageUp") {
          event.preventDefault();
          this.scrollBy(-Math.max(1, this.grid.rows - 1));
          return;
        }
        if (event.key === "PageDown") {
          event.preventDefault();
          this.scrollBy(Math.max(1, this.grid.rows - 1));
          return;
        }
        if (event.key === "Home") {
          event.preventDefault();
          this.scrollToTop();
          return;
        }
        if (event.key === "End") {
          event.preventDefault();
          this.scrollToBottom();
          return;
        }
      }

      const encoded = encodeKeyEvent(event, {
        appCursor: this.wasmTerm.getMode(1, false),
        isMac: IS_MAC,
        altScreen: this.wasmTerm.isAlternateScreen(),
      });
      if (encoded !== null) {
        event.preventDefault();
        event.stopPropagation();
        this.clearSelection();
        this.scrollToBottom();
        this.callbacks.onData?.(encoded);
      }
    });

    // Paste (both images and text).
    this.listen(this.textarea, "paste", (event) => {
      const image = pastedImage(event.clipboardData?.items);
      if (image) {
        event.preventDefault();
        event.stopPropagation();
        this.callbacks.onPasteImage?.(image);
        return;
      }
      const text = event.clipboardData?.getData("text/plain");
      if (text) {
        event.preventDefault();
        event.stopPropagation();
        this.pasteText(text);
      }
    });

    // Track Cmd/Ctrl modifier for clickable link underline feedback.
    const updateModifier = (held: boolean) => {
      if (this.modifierHeld === held) return;
      this.modifierHeld = held;
      this.canvas.style.cursor = held && this.hoveredUrl ? "pointer" : "text";
      if (this.hoveredUrl) this.scheduleRender(true);
    };
    this.listen(window, "keydown", (event) => {
      if (IS_MAC ? event.key === "Meta" : event.key === "Control") updateModifier(true);
    });
    this.listen(window, "keyup", (event) => {
      if (IS_MAC ? event.key === "Meta" : event.key === "Control") updateModifier(false);
    });
    this.listen(window, "blur", () => updateModifier(false));

    // Wheel: zoom, mouse tracking, alt screen arrows, or local scrollback.
    this.listen(this.canvas, "wheel", (event) => {
      event.preventDefault();
      event.stopPropagation();

      if ((IS_MAC ? event.metaKey : event.ctrlKey) && !event.altKey && this.callbacks.onZoom) {
        if (event.deltaY !== 0) this.callbacks.onZoom(event.deltaY < 0 ? 1 : -1);
        return;
      }

      const metrics = this.renderer.getMetrics();

      if (this.isMouseTrackingActive(event)) {
        const notches = this.remoteWheel.take(event.deltaY, event.deltaMode, metrics.height, this.grid.rows);
        if (notches === 0) return;
        const { col, row } = this.pointerToCell(event.clientX, event.clientY);
        const report = wheelReport(notches < 0 ? WHEEL_UP : WHEEL_DOWN, col, row, this.wasmTerm.getMode(1006, false));
        this.callbacks.onData?.(report.repeat(Math.abs(notches)));
        return;
      }

      if (this.wasmTerm.isAlternateScreen()) {
        const notches = this.remoteWheel.take(event.deltaY, event.deltaMode, metrics.height, this.grid.rows);
        if (notches === 0) return;
        const appCursor = this.wasmTerm.getMode(1, false);
        const seq = notches < 0 ? (appCursor ? "\x1bOA" : "\x1b[A") : (appCursor ? "\x1bOB" : "\x1b[B");
        this.callbacks.onData?.(seq.repeat(Math.abs(notches)));
        return;
      }

      const lines = this.localWheel.take(event.deltaY, event.deltaMode, metrics.height, this.grid.rows);
      if (lines !== 0) this.scrollBy(lines);
    }, { passive: false });

    // Mouse down: focus, link click, mouse tracking, or text selection.
    this.listen(this.canvas, "mousedown", (event) => {
      this.focus();

      // Cmd+Click (macOS) or Ctrl+Click (Win/Linux) opens detected URLs.
      const linkModifier = IS_MAC ? event.metaKey : event.ctrlKey;
      if (event.button === 0 && linkModifier) {
        const pos = this.pointerToBufferPos(event.clientX, event.clientY);
        const urlMatch = findUrlAt(this.wasmTerm, pos.absRow, pos.col);
        if (urlMatch) {
          event.preventDefault();
          this.callbacks.onOpenUrl?.(urlMatch.url);
          return;
        }
      }

      // Forward mouse press to terminal app when tracking is enabled.
      if (this.isMouseTrackingActive(event) && (event.button === 0 || event.button === 1 || event.button === 2)) {
        event.preventDefault();
        const button = event.button === 0 ? MOUSE_LEFT : event.button === 1 ? MOUSE_MIDDLE : MOUSE_RIGHT;
        this.mousePressedButton = button;
        const { col, row } = this.pointerToCell(event.clientX, event.clientY);
        this.callbacks.onData?.(
          mouseReport(button, col, row, {
            sgr: this.wasmTerm.getMode(1006, false),
            shift: event.shiftKey,
            alt: event.altKey,
            ctrl: event.ctrlKey,
          }),
        );
        return;
      }

      if (event.button !== 0) return;
      event.preventDefault();

      const pos = this.pointerToBufferPos(event.clientX, event.clientY);

      if (event.detail >= 3) {
        this.selecting = false;
        this.anchorPos = null;
        this.selection = findLineBounds(this.wasmTerm, pos.absRow);
        this.scheduleRender(true);
        return;
      }

      if (event.detail === 2) {
        this.selecting = false;
        this.anchorPos = null;
        const bounds = findWordBounds(this.wasmTerm, pos.absRow, pos.col);
        this.selection = bounds
          ? { start: { absRow: pos.absRow, col: bounds.startCol }, end: { absRow: pos.absRow, col: bounds.endCol } }
          : null;
        this.scheduleRender(true);
        return;
      }

      this.anchorPos = pos;
      this.selecting = true;
      if (this.selection) {
        this.selection = null;
        this.scheduleRender(true);
      }
    });

    // Canvas hover (URL detection & mouse motion reporting).
    this.listen(this.canvas, "mousemove", (event) => {
      if (this.selecting) return;

      if (this.isMouseTrackingActive(event)) {
        const anyMotion = this.wasmTerm.getMode(1003, false);
        const dragMotion = this.wasmTerm.getMode(1002, false) && this.mousePressedButton !== null;
        if (anyMotion || dragMotion) {
          const { col, row } = this.pointerToCell(event.clientX, event.clientY);
          const btn = this.mousePressedButton ?? 3;
          this.callbacks.onData?.(
            mouseReport(btn, col, row, {
              sgr: this.wasmTerm.getMode(1006, false),
              motion: true,
              shift: event.shiftKey,
              alt: event.altKey,
              ctrl: event.ctrlKey,
            }),
          );
          return;
        }
      }

      const linkModifier = IS_MAC ? event.metaKey : event.ctrlKey;
      this.modifierHeld = linkModifier;
      const pos = this.pointerToBufferPos(event.clientX, event.clientY);
      const nextUrl = linkModifier ? findUrlAt(this.wasmTerm, pos.absRow, pos.col) : null;
      const changed =
        nextUrl?.url !== this.hoveredUrl?.url ||
        nextUrl?.absRow !== this.hoveredUrl?.absRow ||
        nextUrl?.startCol !== this.hoveredUrl?.startCol;
      if (changed) {
        this.hoveredUrl = nextUrl;
        this.canvas.style.cursor = nextUrl ? "pointer" : "text";
        this.scheduleRender(true);
      }
    });

    this.listen(this.canvas, "mouseleave", () => {
      if (this.hoveredUrl) {
        this.hoveredUrl = null;
        this.canvas.style.cursor = "text";
        this.scheduleRender(true);
      }
    });

    // Document-wide drag selection & mouse release.
    this.listen(document, "mousemove", (event) => {
      if (!this.selecting || !this.anchorPos) return;
      const rect = this.canvas.getBoundingClientRect();
      if (event.clientY < rect.top) {
        this.startAutoScroll(-1);
      } else if (event.clientY > rect.bottom) {
        this.startAutoScroll(1);
      } else {
        this.stopAutoScroll();
      }
      const clampedX = Math.max(rect.left, Math.min(rect.right - 1, event.clientX));
      const clampedY = Math.max(rect.top, Math.min(rect.bottom - 1, event.clientY));
      const pos = this.pointerToBufferPos(clampedX, clampedY);
      if (pos.absRow === this.anchorPos.absRow && pos.col === this.anchorPos.col) {
        if (this.selection) {
          this.selection = null;
          this.scheduleRender(true);
        }
      } else {
        this.selection = normalizeRange(this.anchorPos, pos);
        this.scheduleRender(true);
      }
    });

    this.listen(document, "mouseup", (event) => {
      if (this.mousePressedButton !== null) {
        const button = this.mousePressedButton;
        this.mousePressedButton = null;
        if (this.wasmTerm.hasMouseTracking()) {
          const { col, row } = this.pointerToCell(event.clientX, event.clientY);
          this.callbacks.onData?.(
            mouseReport(button, col, row, {
              sgr: this.wasmTerm.getMode(1006, false),
              release: true,
              shift: event.shiftKey,
              alt: event.altKey,
              ctrl: event.ctrlKey,
            }),
          );
        }
      }
      if (this.selecting) {
        this.selecting = false;
        this.anchorPos = null;
        this.stopAutoScroll();
      }
    });

    // Context menu (right-click).
    this.listen(this.canvas, "contextmenu", (event) => {
      if (this.isMouseTrackingActive(event)) return;
      event.preventDefault();
      this.callbacks.onContextMenu?.({
        x: event.clientX,
        y: event.clientY,
        hasSelection: this.hasSelection(),
      });
    });
  }
}
