/**
 * Demand-driven high-DPI Canvas 2D renderer for libghostty WASM (`GhosttyTerminal`).
 *
 * Replaces ghostty-web's built-in CanvasRenderer (and the Vite regex patch):
 * - Zero idle rAF loops (renders strictly on demand).
 * - Exact fractional font advance width snapped to device pixels.
 * - Full font ascent/descent + line-height metrics (never clips CJK or descenders).
 * - Geometric device-pixel-snapped box drawing and block elements.
 * - Selection, search match highlights, link hover underlines, and focus-aware cursor.
 */
import { CellFlags, type GhosttyCell, type GhosttyTerminal } from "ghostty-web";
import { TERMINAL_LINE_HEIGHT, type TerminalColors } from "./theme.ts";
import type { BufferRange, SearchMatch, UrlMatch } from "./buffer.ts";

export interface CellMetrics {
  width: number;
  height: number;
  baseline: number;
}

export interface RenderOverlay {
  viewportY: number;
  focused: boolean;
  selection: BufferRange | null;
  matches: SearchMatch[];
  activeMatchIndex: number;
  hoveredUrl: UrlMatch | null;
}

// ------------------------------------------------------------ geometric glyphs

const LIGHT_RULES = new Map<number, number>([
  [0x2500, 0b1010], [0x2502, 0b0101], [0x250c, 0b0110], [0x2510, 0b1100],
  [0x2514, 0b0011], [0x2518, 0b1001], [0x251c, 0b0111], [0x2524, 0b1101],
  [0x252c, 0b1110], [0x2534, 0b1011], [0x253c, 0b1111],
  // Rounded corners fall cleanly onto pixel-snapped joins too.
  [0x256d, 0b0110], [0x256e, 0b1100], [0x2570, 0b0011], [0x256f, 0b1001],
]);

const HEAVY_RULES = new Map<number, number>([
  [0x2501, 0b1010], [0x2503, 0b0101], [0x250f, 0b0110], [0x2513, 0b1100],
  [0x2517, 0b0011], [0x251b, 0b1001], [0x2523, 0b0111], [0x252b, 0b1101],
  [0x2533, 0b1110], [0x253b, 0b1011], [0x254b, 0b1111],
]);

/**
 * Paint box-drawing rules and block elements geometrically at snapped physical
 * pixels so TUI borders join without hairline gaps across DPRs.
 */
export function paintTerminalGlyph(
  ctx: CanvasRenderingContext2D,
  codepoint: number,
  x: number,
  y: number,
  width: number,
  height: number,
  ratio: number,
): boolean {
  if (codepoint < 0x2500 || codepoint > 0x2595) return false;
  const snap = (value: number) => Math.round(value * ratio) / ratio;
  const left = snap(x);
  const top = snap(y);
  const right = snap(x + width);
  const bottom = snap(y + height);
  const rect = (l: number, t: number, r: number, b: number) => {
    if (r > l && b > t) ctx.fillRect(l, t, r - l, b - t);
  };

  // Block elements & shades.
  if (codepoint >= 0x2580) {
    if (codepoint === 0x2580) {
      rect(left, top, right, snap(y + height / 2));
      return true;
    }
    if (codepoint <= 0x2588) {
      rect(left, snap(y + (height * (0x2588 - codepoint)) / 8), right, bottom);
      return true;
    }
    if (codepoint <= 0x258f) {
      rect(left, top, snap(x + (width * (0x2590 - codepoint)) / 8), bottom);
      return true;
    }
    if (codepoint === 0x2590) {
      rect(snap(x + width / 2), top, right, bottom);
      return true;
    }
    if (codepoint >= 0x2591 && codepoint <= 0x2593) {
      const prev = ctx.globalAlpha;
      const alpha = codepoint === 0x2591 ? 0.25 : codepoint === 0x2592 ? 0.5 : 0.75;
      ctx.globalAlpha = prev * alpha;
      rect(left, top, right, bottom);
      ctx.globalAlpha = prev;
      return true;
    }
    if (codepoint === 0x2594) {
      rect(left, top, right, snap(y + height / 8));
      return true;
    }
    if (codepoint === 0x2595) {
      rect(snap(x + (width * 7) / 8), top, right, bottom);
      return true;
    }
    return false;
  }

  // Light, heavy, and rounded box-drawing lines.
  const heavy = HEAVY_RULES.has(codepoint);
  const rule = LIGHT_RULES.get(codepoint) ?? HEAVY_RULES.get(codepoint);
  if (rule === undefined) return false;

  const thickness = Math.max(1, Math.round(width * (heavy ? 0.22 : 0.11) * ratio)) / ratio;
  const centerX = snap(x + (width - thickness) / 2);
  const centerY = snap(y + (height - thickness) / 2);
  const strokeRight = centerX + thickness;
  const strokeBottom = centerY + thickness;

  // Bits clockwise from top: 0b0001 top, 0b0010 right, 0b0100 bottom, 0b1000 left.
  if (rule & 0b0001) rect(centerX, top, strokeRight, centerY);
  if (rule & 0b0100) rect(centerX, strokeBottom, strokeRight, bottom);
  rect(
    rule & 0b1000 ? left : centerX,
    centerY,
    rule & 0b0010 ? right : strokeRight,
    strokeBottom,
  );
  return true;
}

// ----------------------------------------------------------------- glyph atlas

interface AtlasSlot {
  x: number;
  y: number;
  w: number;
  h: number;
  padLeft: number;
}

export type GlyphMeasure = Partial<
  Pick<TextMetrics, "width" | "actualBoundingBoxLeft" | "actualBoundingBoxRight">
>;

/**
 * Compute physical-pixel atlas bounds for a rasterized glyph.
 *
 * Keeps vertical bounds strictly at one row (`metrics.height`) so scroll-copy
 * reuse never bleeds across rows, while expanding horizontal bounds to cover
 * italic overhang or fallback glyphs (e.g. wide symbols, CJK, emoji) whose
 * inked extent exceeds `cellCols * metrics.width`.
 */
export function atlasSlotBounds(
  metrics: CellMetrics,
  cellCols: number,
  italic: boolean,
  dpr: number,
  measured?: GlyphMeasure,
): { padLeft: number; w: number; h: number } {
  let padLeft = italic ? Math.max(1, Math.ceil(metrics.width * 0.12 * dpr)) : 0;
  let contentW =
    Math.ceil(metrics.width * cellCols * dpr) +
    Math.ceil(metrics.width * (italic ? 0.35 : 0.15) * dpr);

  if (measured) {
    const left = typeof measured.actualBoundingBoxLeft === "number" && Number.isFinite(measured.actualBoundingBoxLeft)
      ? measured.actualBoundingBoxLeft
      : 0;
    const right = typeof measured.actualBoundingBoxRight === "number" && Number.isFinite(measured.actualBoundingBoxRight)
      ? measured.actualBoundingBoxRight
      : 0;
    const advance = typeof measured.width === "number" && Number.isFinite(measured.width)
      ? measured.width
      : 0;
    if (left > 0) padLeft = Math.max(padLeft, Math.ceil(left * dpr) + 1);
    const rightPx = Math.ceil(Math.max(advance, right) * dpr) + 1;
    if (rightPx > contentW) contentW = rightPx;
  }

  return {
    padLeft,
    w: Math.max(1, padLeft + contentW),
    h: Math.max(1, Math.ceil(metrics.height * dpr)),
  };
}

const ATLAS_SIZE = 1024;
const ATLAS_MAX_SLOTS = 4096;

/**
 * Offscreen sprite cache for rasterized terminal glyphs.
 *
 * Caching `(codepoint, fontFlags, fgRgb)` as physical-pixel sprites bypasses
 * per-cell CoreText shaping and font-fallback cascade on macOS WKWebView while
 * placing every glyph at its exact device-pixel-snapped cell coordinate with
 * strict vertical row bounds (no inter-row bleed on scroll reuse).
 */
class GlyphAtlas {
  readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly slots = new Map<string, AtlasSlot>();
  private nextX = 0;
  private nextY = 0;
  private rowH = 0;
  private lastFont = "";
  private lastColor = "";

  constructor() {
    this.canvas = document.createElement("canvas");
    this.canvas.width = ATLAS_SIZE;
    this.canvas.height = ATLAS_SIZE;
    const ctx = this.canvas.getContext("2d");
    if (!ctx) throw new Error("Could not acquire 2D context for glyph atlas");
    this.ctx = ctx;
    this.reset();
  }

  reset() {
    this.slots.clear();
    this.nextX = 0;
    this.nextY = 0;
    this.rowH = 0;
    this.lastFont = "";
    this.lastColor = "";
    this.ctx.setTransform(1, 0, 0, 1, 0, 0);
    this.ctx.clearRect(0, 0, ATLAS_SIZE, ATLAS_SIZE);
    this.ctx.textBaseline = "alphabetic";
    this.ctx.textAlign = "left";
  }

  get(
    codepoint: number,
    cellCols: number,
    fontFlags: number,
    font: string,
    rgb: number,
    fgR: number,
    fgG: number,
    fgB: number,
    metrics: CellMetrics,
    dpr: number,
  ): AtlasSlot | null {
    const key = `${codepoint}:${cellCols}:${fontFlags}:${rgb}`;
    const cached = this.slots.get(key);
    if (cached) return cached;

    const ch = String.fromCodePoint(codepoint);
    const ctx = this.ctx;
    const italic = (fontFlags & CellFlags.ITALIC) !== 0;

    let measured: GlyphMeasure | undefined;
    if (codepoint > 0x7e) {
      if (font !== this.lastFont) {
        ctx.font = font;
        this.lastFont = font;
      }
      measured = ctx.measureText(ch);
    }

    const { padLeft, w, h } = atlasSlotBounds(metrics, cellCols, italic, dpr, measured);
    if (w > ATLAS_SIZE || h > ATLAS_SIZE) return null;

    if (this.slots.size >= ATLAS_MAX_SLOTS) this.reset();
    if (this.nextX + w > ATLAS_SIZE) {
      this.nextX = 0;
      this.nextY += this.rowH;
      this.rowH = 0;
    }
    if (this.nextY + h > ATLAS_SIZE) {
      this.reset();
    }

    const x = this.nextX;
    const y = this.nextY;
    this.nextX += w;
    if (h > this.rowH) this.rowH = h;

    if (font !== this.lastFont) {
      ctx.font = font;
      this.lastFont = font;
    }
    const color = `rgb(${fgR},${fgG},${fgB})`;
    if (color !== this.lastColor) {
      ctx.fillStyle = color;
      this.lastColor = color;
    }
    ctx.save();
    ctx.beginPath();
    ctx.rect(x, y, w, h);
    ctx.clip();
    ctx.setTransform(dpr, 0, 0, dpr, x + padLeft, y);
    ctx.fillText(ch, 0, metrics.baseline);
    ctx.restore();

    const slot: AtlasSlot = { x, y, w, h, padLeft };
    this.slots.set(key, slot);
    return slot;
  }
}

// ------------------------------------------------------------- canvas renderer

export class TerminalCanvasRenderer {
  private readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly measureCtx: CanvasRenderingContext2D;
  private readonly atlas: GlyphAtlas;
  private fontSize: number;
  private fontFamily: string;
  private fonts: [string, string, string, string];
  private activeFontFlags = -1;
  private colors: TerminalColors;
  private dpr: number;
  private metrics: CellMetrics;
  private cols = 0;
  private rows = 0;
  private lastViewportY = -1;
  private lastScrollback = -1;
  private lastPlain = false;
  private lastCursor = { x: -1, y: -1, visible: false, focused: false };

  constructor(canvas: HTMLCanvasElement, options: { fontSize: number; fontFamily: string; colors: TerminalColors }) {
    const ctx = canvas.getContext("2d", { alpha: false });
    if (!ctx) throw new Error("Could not acquire 2D canvas context for terminal");
    this.canvas = canvas;
    this.ctx = ctx;
    this.measureCtx = document.createElement("canvas").getContext("2d") ?? ctx;
    this.atlas = new GlyphAtlas();
    this.fontSize = options.fontSize;
    this.fontFamily = options.fontFamily;
    this.fonts = this.buildFontStrings(options.fontSize, options.fontFamily);
    this.colors = options.colors;
    this.dpr = this.currentDpr();
    this.metrics = this.measureFont();
  }

  private buildFontStrings(size: number, family: string): [string, string, string, string] {
    return [
      `400 ${size}px ${family}`,
      `700 ${size}px ${family}`,
      `italic 400 ${size}px ${family}`,
      `italic 700 ${size}px ${family}`,
    ];
  }

  private currentDpr(): number {
    return typeof window !== "undefined" && window.devicePixelRatio > 0 ? window.devicePixelRatio : 1;
  }

  getMetrics(): CellMetrics {
    return this.metrics;
  }

  setColors(colors: TerminalColors) {
    this.colors = colors;
  }

  setFont(fontSize: number, fontFamily = this.fontFamily) {
    this.fontSize = fontSize;
    this.fontFamily = fontFamily;
    this.fonts = this.buildFontStrings(fontSize, fontFamily);
    this.activeFontFlags = -1;
    this.dpr = this.currentDpr();
    this.metrics = this.measureFont();
    this.atlas.reset();
  }

  remeasure() {
    this.dpr = this.currentDpr();
    this.metrics = this.measureFont();
    this.activeFontFlags = -1;
    this.atlas.reset();
  }

  private measureFont(): CellMetrics {
    const ctx = this.measureCtx;
    ctx.font = this.fonts[0];
    // Average across 10 cells to avoid rounding drift from a single glyph.
    const sample = ctx.measureText("MMMMMMMMMM");
    const rawWidth = sample.width > 0 ? sample.width / 10 : this.fontSize * 0.6;
    const width = Math.max(4, Math.round(rawWidth * this.dpr) / this.dpr);

    const ascent = sample.fontBoundingBoxAscent || sample.actualBoundingBoxAscent || this.fontSize * 0.8;
    const descent = sample.fontBoundingBoxDescent || sample.actualBoundingBoxDescent || this.fontSize * 0.2;
    const height = Math.max(Math.ceil(ascent + descent), Math.round(this.fontSize * TERMINAL_LINE_HEIGHT));
    const baseline = Math.round(ascent + (height - ascent - descent) / 2);
    return { width, height, baseline };
  }

  private ensureCanvasSize(cols: number, rows: number): boolean {
    const dpr = this.currentDpr();
    const cssWidth = cols * this.metrics.width;
    const cssHeight = rows * this.metrics.height;
    const backingWidth = Math.max(1, Math.round(cssWidth * dpr));
    const backingHeight = Math.max(1, Math.round(cssHeight * dpr));

    if (
      dpr === this.dpr &&
      cols === this.cols &&
      rows === this.rows &&
      this.canvas.width === backingWidth &&
      this.canvas.height === backingHeight
    ) {
      return false;
    }

    if (dpr !== this.dpr) {
      this.dpr = dpr;
      this.metrics = this.measureFont();
      this.atlas.reset();
    }

    this.cols = cols;
    this.rows = rows;
    this.activeFontFlags = -1;
    const w = cols * this.metrics.width;
    const h = rows * this.metrics.height;
    this.canvas.style.width = `${w}px`;
    this.canvas.style.height = `${h}px`;
    this.canvas.width = Math.max(1, Math.round(w * this.dpr));
    this.canvas.height = Math.max(1, Math.round(h * this.dpr));
    this.ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    this.ctx.textBaseline = "alphabetic";
    this.ctx.textAlign = "left";
    this.ctx.fillStyle = this.colors.background;
    this.ctx.fillRect(0, 0, w, h);
    return true;
  }

  /** Render the current WASM buffer + overlays. Returns true if anything was painted. */
  render(term: GhosttyTerminal, overlay: RenderOverlay, forceFull = false): boolean {
    const dirtyState = term.update();
    const { cols, rows } = term.getDimensions();
    const resized = this.ensureCanvasSize(cols, rows);
    const viewportY = Math.max(0, Math.floor(overlay.viewportY));
    const scrollback = term.getScrollbackLength();
    const cursor = term.getCursor();
    const defaultColors = term.getColors();
    const plain = overlay.selection === null && overlay.matches.length === 0 && overlay.hoveredUrl === null;
    const scrolledHistoryShifted = viewportY > 0 && scrollback !== this.lastScrollback;

    const full =
      forceFull ||
      resized ||
      dirtyState === 2 ||
      viewportY !== this.lastViewportY ||
      scrolledHistoryShifted ||
      !plain || !this.lastPlain;

    const cursorChanged =
      cursor.x !== this.lastCursor.x ||
      cursor.y !== this.lastCursor.y ||
      cursor.visible !== this.lastCursor.visible ||
      overlay.focused !== this.lastCursor.focused;

    if (!full && dirtyState === 0 && !cursorChanged) {
      return false;
    }

    const shift = this.lastViewportY - viewportY;
    // With unchanged cells and no cursor or overlays, keep the rows already
    // on the canvas. Only expose new rows at the edge of the viewport.
    const shifted = !forceFull && !resized && dirtyState === 0 && plain && this.lastPlain
      && scrollback === this.lastScrollback && viewportY > 0 && this.lastViewportY > 0
      && Number.isInteger(this.metrics.height * this.dpr)
      && shift !== 0 && Math.abs(shift) < rows;
    const firstRow = shifted && shift > 0 ? rows - shift : 0;
    const endRow = shifted && shift < 0 ? -shift : rows;
    if (shifted) this.shiftRows(shift, rows);

    // Scrollback-only frames need no cells from the active screen.
    let viewport: GhosttyCell[] | undefined;

    // Index search matches by visible row for O(1) lookup.
    const matchesByRow = new Map<number, { startCol: number; endCol: number; active: boolean }[]>();
    if (overlay.matches.length > 0) {
      const topAbs = scrollback - viewportY;
      const bottomAbs = topAbs + rows - 1;
      for (let i = 0; i < overlay.matches.length; i++) {
        const match = overlay.matches[i];
        if (match.absRow < topAbs || match.absRow > bottomAbs) continue;
        const visRow = match.absRow - topAbs;
        let list = matchesByRow.get(visRow);
        if (!list) {
          list = [];
          matchesByRow.set(visRow, list);
        }
        list.push({ startCol: match.startCol, endCol: match.endCol, active: i === overlay.activeMatchIndex });
      }
    }

    const activeSelection = overlay.selection;

    for (let visRow = firstRow; visRow < endRow; visRow++) {
      const absRow = scrollback - viewportY + visRow;
      if (
        !full &&
        !term.isRowDirty(visRow) &&
        visRow !== cursor.y &&
        visRow !== this.lastCursor.y
      ) {
        continue;
      }

      let cells: GhosttyCell[] | null = null;
      let startIndex = 0;
      if (absRow < scrollback) {
        cells = term.getScrollbackLine(absRow);
      } else {
        const screenRow = absRow - scrollback;
        if (screenRow >= 0 && screenRow < rows) {
          cells = viewport ??= term.getViewport();
          startIndex = screenRow * cols;
        }
      }

      this.renderRow(
        term,
        cells,
        startIndex,
        cols,
        visRow,
        absRow,
        scrollback,
        defaultColors.background,
        activeSelection,
        matchesByRow.get(visRow),
        overlay.hoveredUrl && overlay.hoveredUrl.absRow === absRow ? overlay.hoveredUrl : null,
        viewportY > 0,
      );
    }

    // Cursor only renders at the live bottom viewport.
    if (viewportY === 0 && cursor.visible && cursor.x >= 0 && cursor.x < cols && cursor.y >= 0 && cursor.y < rows) {
      this.renderCursor(cursor.x, cursor.y, overlay.focused);
    }

    this.lastViewportY = viewportY;
    this.lastScrollback = scrollback;
    this.lastPlain = plain;
    this.lastCursor = { x: cursor.x, y: cursor.y, visible: cursor.visible, focused: overlay.focused };
    term.markClean();
    return true;
  }

  private shiftRows(shift: number, rows: number) {
    const height = (rows - Math.abs(shift)) * this.metrics.height;
    const source = Math.max(0, shift) * this.metrics.height;
    const target = Math.max(0, -shift) * this.metrics.height;
    this.ctx.drawImage(this.canvas, 0, source * this.dpr, this.canvas.width, height * this.dpr,
      0, target, this.canvas.width / this.dpr, height);
  }

  private renderRow(
    term: GhosttyTerminal,
    cells: GhosttyCell[] | null,
    offset: number,
    cols: number,
    visRow: number,
    absRow: number,
    scrollback: number,
    defaultBg: { r: number; g: number; b: number },
    selection: BufferRange | null,
    rowMatches: { startCol: number; endCol: number; active: boolean }[] | undefined,
    hoveredUrl: UrlMatch | null,
    clipGlyphs: boolean,
  ) {
    const ctx = this.ctx;
    const { width: cellW, height: cellH, baseline } = this.metrics;
    const dpr = this.dpr;
    const y = visRow * cellH;

    // Clear full row with theme background.
    ctx.fillStyle = this.colors.background;
    ctx.fillRect(0, y, cols * cellW, cellH);

    if (!cells) return;
    const limit = Math.min(cols, cells.length - offset);

    // Compute selection span on this row, if any.
    let selStart = -1;
    let selEnd = -1;
    if (selection && absRow >= selection.start.absRow && absRow <= selection.end.absRow) {
      selStart = absRow === selection.start.absRow ? selection.start.col : 0;
      selEnd = absRow === selection.end.absRow ? selection.end.col : cols - 1;
    }

    // Pass 1: coalesced cell background runs.
    let runRgb = -1;
    let runStart = 0;
    let runEnd = 0;
    let paintedRgb = -1;
    const flushBgRun = () => {
      if (runRgb < 0) return;
      if (runRgb !== paintedRgb) {
        ctx.fillStyle = `rgb(${(runRgb >> 16) & 255},${(runRgb >> 8) & 255},${runRgb & 255})`;
        paintedRgb = runRgb;
      }
      ctx.fillRect(runStart * cellW, y, (runEnd - runStart) * cellW, cellH);
      runRgb = -1;
    };
    for (let col = 0; col < limit; col++) {
      const cell = cells[offset + col];
      if (!cell || cell.width === 0) continue;

      const inverse = (cell.flags & CellFlags.INVERSE) !== 0;
      const bgR = inverse ? cell.fg_r : cell.bg_r;
      const bgG = inverse ? cell.fg_g : cell.bg_g;
      const bgB = inverse ? cell.fg_b : cell.bg_b;

      if (inverse || bgR !== defaultBg.r || bgG !== defaultBg.g || bgB !== defaultBg.b) {
        const rgb = (bgR << 16) | (bgG << 8) | bgB;
        if (rgb === runRgb && col === runEnd) {
          runEnd = col + cell.width;
        } else {
          flushBgRun();
          runRgb = rgb;
          runStart = col;
          runEnd = col + cell.width;
        }
      } else {
        flushBgRun();
      }
    }
    flushBgRun();

    // Pass 1b: search highlights.
    if (rowMatches) {
      for (const match of rowMatches) {
        const x = match.startCol * cellW;
        const w = (match.endCol - match.startCol + 1) * cellW;
        ctx.fillStyle = match.active ? this.colors.activeMatchBackground : this.colors.matchBackground;
        ctx.fillRect(x, y, w, cellH);
      }
    }

    // Pass 1c: selection highlight.
    if (selStart >= 0 && selEnd >= selStart) {
      ctx.fillStyle = this.colors.selectionBackground;
      ctx.fillRect(selStart * cellW, y, (selEnd - selStart + 1) * cellW, cellH);
    }

    // Pass 2: glyphs (via sprite atlas for single-codepoint cells) & decorations.
    let paint = -1;
    const decoHeight = Math.max(1, Math.round(dpr) / dpr);
    for (let col = 0; col < limit; col++) {
      const cell = cells[offset + col];
      if (!cell || cell.width === 0 || (cell.flags & CellFlags.INVISIBLE) !== 0) continue;

      const hasGlyph = cell.codepoint > 32 || cell.grapheme_len > 0;
      const hasUnderline = (cell.flags & CellFlags.UNDERLINE) !== 0;
      const hasStrike = (cell.flags & CellFlags.STRIKETHROUGH) !== 0;
      const isHoveredLink = hoveredUrl !== null && col >= hoveredUrl.startCol && col <= hoveredUrl.endCol;

      if (!hasGlyph && !hasUnderline && !hasStrike && !isHoveredLink) continue;

      const x = col * cellW;
      const w = cellW * cell.width;
      const inverse = (cell.flags & CellFlags.INVERSE) !== 0;
      const fgR = inverse ? cell.bg_r : cell.fg_r;
      const fgG = inverse ? cell.bg_g : cell.fg_g;
      const fgB = inverse ? cell.bg_b : cell.fg_b;
      const faint = (cell.flags & CellFlags.FAINT) !== 0;
      const rgb = (fgR << 16) | (fgG << 8) | fgB;

      if (faint) ctx.globalAlpha = 0.55;

      if (hasGlyph) {
        const isGeometric = cell.grapheme_len === 0 && cell.codepoint >= 0x2500 && cell.codepoint <= 0x2595;
        let paintedGeometric = false;
        if (isGeometric) {
          if (rgb !== paint) {
            ctx.fillStyle = `rgb(${fgR},${fgG},${fgB})`;
            paint = rgb;
          }
          paintedGeometric = paintTerminalGlyph(ctx, cell.codepoint, x, y, w, cellH, dpr);
        }

        if (!paintedGeometric) {
          const fontFlags = cell.flags & (CellFlags.ITALIC | CellFlags.BOLD);
          const font = this.fonts[fontFlags];
          const slot =
            cell.grapheme_len === 0
              ? this.atlas.get(
                  cell.codepoint,
                  cell.width,
                  fontFlags,
                  font,
                  rgb,
                  fgR,
                  fgG,
                  fgB,
                  this.metrics,
                  dpr,
                )
              : null;

          if (slot) {
            ctx.drawImage(
              this.atlas.canvas,
              slot.x,
              slot.y,
              slot.w,
              slot.h,
              x - slot.padLeft / dpr,
              y,
              slot.w / dpr,
              slot.h / dpr,
            );
          } else {
            if (rgb !== paint) {
              ctx.fillStyle = `rgb(${fgR},${fgG},${fgB})`;
              paint = rgb;
            }
            if (fontFlags !== this.activeFontFlags) {
              ctx.font = font;
              this.activeFontFlags = fontFlags;
            }
            const text =
              cell.grapheme_len > 0
                ? absRow < scrollback
                  ? term.getScrollbackGraphemeString(absRow, col)
                  : term.getGraphemeString(absRow - scrollback, col)
                : String.fromCodePoint(cell.codepoint);
            if (clipGlyphs) {
              ctx.save();
              ctx.beginPath();
              ctx.rect(0, y, cols * cellW, cellH);
              ctx.clip();
              ctx.fillText(text, x, y + baseline);
              ctx.restore();
            } else {
              ctx.fillText(text, x, y + baseline);
            }
          }
        }
      }

      if (hasUnderline || isHoveredLink) {
        const lineY = Math.round((y + baseline + 2) * dpr) / dpr;
        if (isHoveredLink) {
          ctx.fillStyle = this.colors.link;
          paint = -1;
        } else if (rgb !== paint) {
          ctx.fillStyle = `rgb(${fgR},${fgG},${fgB})`;
          paint = rgb;
        }
        ctx.fillRect(x, lineY, w, decoHeight);
      }

      if (hasStrike) {
        const lineY = Math.round((y + cellH * 0.5) * dpr) / dpr;
        if (rgb !== paint) {
          ctx.fillStyle = `rgb(${fgR},${fgG},${fgB})`;
          paint = rgb;
        }
        ctx.fillRect(x, lineY, w, decoHeight);
      }

      if (faint) ctx.globalAlpha = 1;
    }
  }

  private renderCursor(col: number, row: number, focused: boolean) {
    const ctx = this.ctx;
    const { width: cellW, height: cellH } = this.metrics;
    const x = col * cellW;
    const y = row * cellH;
    ctx.fillStyle = this.colors.cursor;

    if (focused) {
      const barWidth = Math.max(2, Math.round(2 * this.dpr) / this.dpr);
      ctx.fillRect(x, y, barWidth, cellH);
    } else {
      const stroke = Math.max(1, Math.round(this.dpr) / this.dpr);
      ctx.strokeStyle = this.colors.cursor;
      ctx.lineWidth = stroke;
      ctx.strokeRect(x + stroke / 2, y + stroke / 2, cellW - stroke, cellH - stroke);
    }
  }

  clear() {
    this.ctx.fillStyle = this.colors.background;
    this.ctx.fillRect(0, 0, this.cols * this.metrics.width, this.rows * this.metrics.height);
  }
}
