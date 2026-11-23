export const TERMINAL_FONT = '"JetBrains Mono", "SF Mono", Consolas, "Liberation Mono", ui-monospace, "PingFang SC", "Hiragino Sans", "Microsoft YaHei", monospace';
export const TERMINAL_FONT_DEFAULT = 14;
export const TERMINAL_FONT_MIN = 10;
export const TERMINAL_FONT_MAX = 24;
export const TERMINAL_LINE_HEIGHT = 1.2;
// Ghostty draws an 8 px scrollbar inset by 4 px, with a 2 px backing margin.
export const TERMINAL_SCROLLBAR_GUTTER = 14;
const FONT_SIZE_KEY = "cabin.terminalFontSize";

export function terminalFontSize(size: number): number {
  if (!Number.isFinite(size)) return TERMINAL_FONT_DEFAULT;
  return Math.max(TERMINAL_FONT_MIN, Math.min(TERMINAL_FONT_MAX, Math.round(size)));
}

export function readTerminalFontSize(): number {
  try {
    return terminalFontSize(Number(localStorage.getItem(FONT_SIZE_KEY) ?? TERMINAL_FONT_DEFAULT));
  } catch {
    return TERMINAL_FONT_DEFAULT;
  }
}

export function saveTerminalFontSize(size: number) {
  try { localStorage.setItem(FONT_SIZE_KEY, String(size)); } catch { /* Applies for this view. */ }
}

let fontsReady: Promise<void> | undefined;

/** Canvas does not trigger web-font loading reliably across webviews. */
export function loadTerminalFonts(): Promise<void> {
  fontsReady ??= Promise.allSettled(["400", "700", "italic 400", "italic 700"].map((style) =>
    document.fonts.load(`${style} ${TERMINAL_FONT_DEFAULT}px "JetBrains Mono"`, "M0@"),
  )).then(() => undefined);
  return fontsReady;
}
