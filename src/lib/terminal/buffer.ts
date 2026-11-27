/**
 * Unified buffer inspection across Ghostty's scrollback + active screen:
 * text extraction (wide-char & soft-wrap aware), double/triple-click selection,
 * scrollback search, and URL detection.
 */
import type { GhosttyCell } from "ghostty-web";

export interface TerminalBufferSource {
  readonly cols: number;
  readonly rows: number;
  getScrollbackLength(): number;
  getScrollbackLine(offset: number): GhosttyCell[] | null;
  getLine(row: number): GhosttyCell[] | null;
  isRowWrapped(row: number): boolean;
  getGraphemeString(row: number, col: number): string;
  getScrollbackGraphemeString(offset: number, col: number): string;
}

export interface BufferPos {
  /** Absolute row index: `0..scrollbackLen-1` is scrollback, `scrollbackLen..` is active screen. */
  absRow: number;
  /** 0-based column index. */
  col: number;
}

export interface BufferRange {
  start: BufferPos;
  end: BufferPos;
}

export interface SearchMatch {
  absRow: number;
  startCol: number;
  endCol: number;
}

export interface UrlMatch {
  url: string;
  absRow: number;
  startCol: number;
  endCol: number;
}

/** Order two positions so `start <= end`. */
export function normalizeRange(a: BufferPos, b: BufferPos): BufferRange {
  if (a.absRow > b.absRow || (a.absRow === b.absRow && a.col > b.col)) {
    return { start: b, end: a };
  }
  return { start: a, end: b };
}

/** Read cells for any absolute row (scrollback or active screen). */
export function getAbsoluteLine(term: TerminalBufferSource, absRow: number): GhosttyCell[] | null {
  const scrollback = term.getScrollbackLength();
  if (absRow < 0 || absRow >= scrollback + term.rows) return null;
  if (absRow < scrollback) return term.getScrollbackLine(absRow);
  return term.getLine(absRow - scrollback);
}

function cellText(term: TerminalBufferSource, absRow: number, col: number, cell: GhosttyCell, scrollback: number): string {
  if (cell.width === 0) return "";
  if (cell.grapheme_len > 0) {
    return absRow < scrollback
      ? term.getScrollbackGraphemeString(absRow, col)
      : term.getGraphemeString(absRow - scrollback, col);
  }
  if (!cell.codepoint || cell.codepoint < 32) return " ";
  return String.fromCodePoint(cell.codepoint);
}

export interface LineTextMap {
  /** Trimmed-right string representation of the row. */
  text: string;
  /** Maps each character index in `text` back to its 0-based terminal column. */
  indexToCol: number[];
  /** Column end for each character index (inclusive; handles width-2 CJK cells). */
  indexToEndCol: number[];
}

/** Convert an absolute row into text plus bidirectional column mappings. */
export function getAbsoluteLineText(term: TerminalBufferSource, absRow: number): LineTextMap {
  const scrollback = term.getScrollbackLength();
  const cells = getAbsoluteLine(term, absRow);
  if (!cells) return { text: "", indexToCol: [], indexToEndCol: [] };

  let text = "";
  const indexToCol: number[] = [];
  const indexToEndCol: number[] = [];
  let lastNonSpaceLen = 0;

  for (let col = 0; col < cells.length; col++) {
    const cell = cells[col];
    if (!cell || cell.width === 0) continue;
    const ch = cellText(term, absRow, col, cell, scrollback);
    const endCol = Math.min(cells.length - 1, col + Math.max(1, cell.width) - 1);
    for (let i = 0; i < ch.length; i++) {
      indexToCol.push(col);
      indexToEndCol.push(endCol);
    }
    text += ch;
    if (cell.codepoint && cell.codepoint !== 32) {
      lastNonSpaceLen = text.length;
    }
  }

  return {
    text: text.slice(0, lastNonSpaceLen),
    indexToCol: indexToCol.slice(0, lastNonSpaceLen),
    indexToEndCol: indexToEndCol.slice(0, lastNonSpaceLen),
  };
}

/** Extract selected text across scrollback + screen, joining soft-wrapped active lines. */
export function extractSelectionText(term: TerminalBufferSource, a: BufferPos, b: BufferPos): string {
  const { start, end } = normalizeRange(a, b);
  const scrollback = term.getScrollbackLength();
  const lines: string[] = [];

  for (let absRow = start.absRow; absRow <= end.absRow; absRow++) {
    const cells = getAbsoluteLine(term, absRow);
    if (!cells) continue;
    const from = absRow === start.absRow ? Math.max(0, start.col) : 0;
    const to = absRow === end.absRow ? Math.min(cells.length - 1, end.col) : cells.length - 1;

    let rowText = "";
    let lastNonSpace = 0;
    for (let col = from; col <= to; col++) {
      const cell = cells[col];
      if (!cell || cell.width === 0) continue;
      // If selection starts on the continuation half of a wide char, back up one col.
      const ch = cellText(term, absRow, col, cell, scrollback);
      rowText += ch;
      if (cell.codepoint && cell.codepoint !== 32) {
        lastNonSpace = rowText.length;
      }
    }

    const wrapped =
      absRow >= scrollback &&
      absRow < end.absRow &&
      term.isRowWrapped(absRow - scrollback);

    if (wrapped) {
      lines.push(rowText);
    } else {
      lines.push(rowText.slice(0, lastNonSpace));
    }

    if (absRow < end.absRow && !wrapped) {
      lines.push("\n");
    }
  }

  return lines.join("");
}

const WORD_CHAR = /[\w./:@~+-]/;
const CJK_CHAR = /[\u3040-\u30ff\u3400-\u9fff\uf900-\ufaff\uac00-\ud7af]/;

function charClass(codepoint: number): number {
  if (!codepoint || codepoint <= 32) return 0;
  const ch = String.fromCodePoint(codepoint);
  if (CJK_CHAR.test(ch)) return 2;
  if (WORD_CHAR.test(ch)) return 1;
  return 3;
}

/** Double-click word bounds on ANY row (scrollback or active screen). */
export function findWordBounds(term: TerminalBufferSource, absRow: number, col: number): { startCol: number; endCol: number } | null {
  const cells = getAbsoluteLine(term, absRow);
  if (!cells || cells.length === 0) return null;
  let c = Math.max(0, Math.min(col, cells.length - 1));
  if (cells[c]?.width === 0 && c > 0) c -= 1;

  const target = charClass(cells[c]?.codepoint ?? 0);
  if (target === 0) return null;

  let startCol = c;
  while (startCol > 0) {
    let prev = startCol - 1;
    if (cells[prev]?.width === 0 && prev > 0) prev -= 1;
    if (charClass(cells[prev]?.codepoint ?? 0) !== target) break;
    startCol = prev;
  }

  let endCol = c + Math.max(1, cells[c]?.width ?? 1) - 1;
  while (endCol + 1 < cells.length) {
    const next = endCol + 1;
    if (charClass(cells[next]?.codepoint ?? 0) !== target) break;
    endCol = next + Math.max(1, cells[next]?.width ?? 1) - 1;
  }

  return { startCol, endCol: Math.min(endCol, cells.length - 1) };
}

/** Triple-click logical line bounds (expands across soft-wrapped rows). */
export function findLineBounds(term: TerminalBufferSource, absRow: number): BufferRange {
  const scrollback = term.getScrollbackLength();
  const maxRow = scrollback + term.rows - 1;
  let startRow = Math.max(0, Math.min(absRow, maxRow));
  let endRow = startRow;

  while (startRow > scrollback && term.isRowWrapped(startRow - 1 - scrollback)) {
    startRow -= 1;
  }
  while (endRow >= scrollback && endRow < maxRow && term.isRowWrapped(endRow - scrollback)) {
    endRow += 1;
  }

  return {
    start: { absRow: startRow, col: 0 },
    end: { absRow: endRow, col: Math.max(0, term.cols - 1) },
  };
}

/** Search scrollback + active screen for `query`. */
export function searchTerminalBuffer(
  term: TerminalBufferSource,
  query: string,
  options: { caseSensitive?: boolean; maxMatches?: number } = {},
): SearchMatch[] {
  if (!query) return [];
  const caseSensitive = options.caseSensitive ?? false;
  const maxMatches = options.maxMatches ?? 1000;
  const needle = caseSensitive ? query : query.toLowerCase();
  const totalRows = term.getScrollbackLength() + term.rows;
  const matches: SearchMatch[] = [];

  for (let absRow = 0; absRow < totalRows; absRow++) {
    const { text, indexToCol, indexToEndCol } = getAbsoluteLineText(term, absRow);
    if (!text) continue;
    const haystack = caseSensitive ? text : text.toLowerCase();
    let from = 0;
    while (from <= haystack.length - needle.length) {
      const idx = haystack.indexOf(needle, from);
      if (idx === -1) break;
      const endIdx = idx + needle.length - 1;
      matches.push({
        absRow,
        startCol: indexToCol[idx] ?? 0,
        endCol: indexToEndCol[endIdx] ?? indexToCol[idx] ?? 0,
      });
      if (matches.length >= maxMatches) return matches;
      from = idx + Math.max(1, needle.length);
    }
  }

  return matches;
}

const URL_REGEX = /\b(?:https?:\/\/|mailto:|ssh:\/\/|git:\/\/|ftp:\/\/)[^\s<>'"`]+/gi;
const TRAILING_PUNCT = /[.,;!?:'"\]}>)]+$/;

/** Detect a URL under `(absRow, col)` for Cmd/Ctrl-click navigation. */
export function findUrlAt(term: TerminalBufferSource, absRow: number, col: number): UrlMatch | null {
  const { text, indexToCol, indexToEndCol } = getAbsoluteLineText(term, absRow);
  if (!text) return null;

  URL_REGEX.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = URL_REGEX.exec(text)) !== null) {
    let raw = match[0];
    // Preserve balanced closing parentheses (e.g. Wikipedia URLs).
    while (TRAILING_PUNCT.test(raw)) {
      const last = raw[raw.length - 1];
      if (last === ")" && (raw.match(/\(/g)?.length ?? 0) >= (raw.match(/\)/g)?.length ?? 0)) {
        break;
      }
      raw = raw.slice(0, -1);
    }
    if (raw.length < 8) continue;
    const startCol = indexToCol[match.index] ?? 0;
    const endCol = indexToEndCol[match.index + raw.length - 1] ?? startCol;
    if (col >= startCol && col <= endCol) {
      return { url: raw, absRow, startCol, endCol };
    }
  }
  return null;
}
