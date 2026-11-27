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

// ------------------------------------------------------------- canvas renderer

export class TerminalCanvasRenderer {
  private readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly measureCtx: CanvasRenderingContext2D;
  private fontSize: number;
  private fontFamily: string;
  private colors: TerminalColors;
  private dpr: number;
  private metrics: CellMetrics;
  private cols = 0;
  private rows = 0;
  private lastViewportY = -1;
  private lastCursor = { x: -1, y: -1, focused: false };

  constructor(canvas: HTMLCanvasElement, options: { fontSize: number; fontFamily: string; colors: TerminalColors }) {
    const ctx = canvas.getContext("2d", { alpha: false });
    if (!ctx) throw new Error("Could not acquire 2D canvas context for terminal");
    this.canvas = canvas;
    this.ctx = ctx;
    this.measureCtx = document.createElement("canvas").getContext("2d") ?? ctx;
    this.fontSize = options.fontSize;
    this.fontFamily = options.fontFamily;
    this.colors = options.colors;
    this.dpr = this.currentDpr();
    this.metrics = this.measureFont();
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
    this.dpr = this.currentDpr();
    this.metrics = this.measureFont();
  }

  remeasure() {
    this.dpr = this.currentDpr();
    this.metrics = this.measureFont();
  }

  private measureFont(): CellMetrics {
    const ctx = this.measureCtx;
    ctx.font = `400 ${this.fontSize}px ${this.fontFamily}`;
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
    }

    this.cols = cols;
    this.rows = rows;
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

    const full =
      forceFull ||
      resized ||
      dirtyState === 2 ||
      viewportY !== this.lastViewportY ||
      overlay.selection !== null ||
      overlay.matches.length > 0 ||
      overlay.hoveredUrl !== null;

    const cursorChanged =
      cursor.x !== this.lastCursor.x ||
      cursor.y !== this.lastCursor.y ||
      overlay.focused !== this.lastCursor.focused;

    if (!full && dirtyState === 0 && !cursorChanged) {
      return false;
    }

    // Grab the whole active screen in one WASM crossing.
    const viewport = term.getViewport();

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

    for (let visRow = 0; visRow < rows; visRow++) {
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
          cells = viewport;
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
      );
    }

    // Cursor only renders at the live bottom viewport.
    if (viewportY === 0 && cursor.visible && cursor.x >= 0 && cursor.x < cols && cursor.y >= 0 && cursor.y < rows) {
      this.renderCursor(cursor.x, cursor.y, overlay.focused);
    }

    this.lastViewportY = viewportY;
    this.lastCursor = { x: cursor.x, y: cursor.y, focused: overlay.focused };
    term.markClean();
    return true;
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
  ) {
    const ctx = this.ctx;
    const { width: cellW, height: cellH, baseline } = this.metrics;
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

    // Pass 1: cell backgrounds.
    for (let col = 0; col < limit; col++) {
      const cell = cells[offset + col];
      if (!cell || cell.width === 0) continue;
      const x = col * cellW;
      const w = cellW * cell.width;

      const inverse = (cell.flags & CellFlags.INVERSE) !== 0;
      const bgR = inverse ? cell.fg_r : cell.bg_r;
      const bgG = inverse ? cell.fg_g : cell.bg_g;
      const bgB = inverse ? cell.fg_b : cell.bg_b;

      if (inverse || bgR !== defaultBg.r || bgG !== defaultBg.g || bgB !== defaultBg.b) {
        ctx.fillStyle = `rgb(${bgR},${bgG},${bgB})`;
        ctx.fillRect(x, y, w, cellH);
      }
    }

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

    // Pass 2: glyphs & decorations.
    let currentFont = "";
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

      ctx.fillStyle = `rgb(${fgR},${fgG},${fgB})`;
      if (faint) ctx.globalAlpha = 0.55;

      if (hasGlyph) {
        const paintedGeometric =
          cell.grapheme_len === 0 &&
          paintTerminalGlyph(ctx, cell.codepoint, x, y, w, cellH, this.dpr);

        if (!paintedGeometric) {
          const italic = (cell.flags & CellFlags.ITALIC) !== 0 ? "italic " : "";
          const bold = (cell.flags & CellFlags.BOLD) !== 0 ? "700 " : "400 ";
          const font = `${italic}${bold}${this.fontSize}px ${this.fontFamily}`;
          if (font !== currentFont) {
            ctx.font = font;
            currentFont = font;
          }
          const text =
            cell.grapheme_len > 0
              ? absRow < scrollback
                ? term.getScrollbackGraphemeString(absRow, col)
                : term.getGraphemeString(absRow - scrollback, col)
              : String.fromCodePoint(cell.codepoint);
          ctx.fillText(text, x, y + baseline);
        }
      }

      if (hasUnderline || isHoveredLink) {
        const lineY = Math.round((y + baseline + 2) * this.dpr) / this.dpr;
        ctx.fillStyle = isHoveredLink ? this.colors.link : `rgb(${fgR},${fgG},${fgB})`;
        ctx.fillRect(x, lineY, w, Math.max(1, Math.round(this.dpr) / this.dpr));
      }

      if (hasStrike) {
        const lineY = Math.round((y + cellH * 0.5) * this.dpr) / this.dpr;
        ctx.fillRect(x, lineY, w, Math.max(1, Math.round(this.dpr) / this.dpr));
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
