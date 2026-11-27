/**
 * Terminal colour palettes and reactive font appearance preferences.
 * Shared across every terminal surface so changing size or scheme updates
 * all mounted and pooled terminals immediately.
 */

export type ColorScheme = "light" | "dark";

export interface AnsiPalette {
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
}

export interface TerminalColors extends AnsiPalette {
  background: string;
  foreground: string;
  cursor: string;
  cursorAccent: string;
  selectionBackground: string;
  selectionForeground: string;
  matchBackground: string;
  activeMatchBackground: string;
  link: string;
}

export const BASE: Record<ColorScheme, { background: string; foreground: string; accent: string }> = {
  light: { background: "#f8fafb", foreground: "#222d39", accent: "#266a78" },
  dark: { background: "#171b21", foreground: "#e0e8ef", accent: "#82c8cf" },
};

export const ANSI: Record<ColorScheme, AnsiPalette> = {
  light: {
    black: "#24292f",
    red: "#b42318",
    green: "#1a7f37",
    yellow: "#875c00",
    blue: "#0b5cad",
    magenta: "#8250df",
    cyan: "#0f6e78",
    // "White" is the dim grey that light themes use for de-emphasised text.
    white: "#5f6873",
    brightBlack: "#59636e",
    brightRed: "#9a1b10",
    brightGreen: "#116329",
    brightYellow: "#6b4a00",
    brightBlue: "#0550ae",
    brightMagenta: "#6639ba",
    brightCyan: "#0b5961",
    brightWhite: "#6e7781",
  },
  dark: {
    black: "#3a3834",
    red: "#e2675a",
    green: "#93c46d",
    yellow: "#e0a84b",
    blue: "#79a6dc",
    magenta: "#c79bd9",
    cyan: "#6fc3c0",
    white: "#d6d3cd",
    brightBlack: "#8a857d",
    brightRed: "#f08a7e",
    brightGreen: "#aedb8a",
    brightYellow: "#f2c473",
    brightBlue: "#9cc2ee",
    brightMagenta: "#dbb6ea",
    brightCyan: "#92dbd8",
    brightWhite: "#f5f3ef",
  },
};

/** Full terminal theme for a scheme; optional `base` overrides come from tokens. */
export function terminalColors(
  scheme: ColorScheme,
  base: Partial<{ background: string; foreground: string; accent: string }> = {},
): TerminalColors {
  const { background, foreground, accent } = { ...BASE[scheme], ...filled(base) };
  return {
    ...ANSI[scheme],
    background,
    foreground,
    cursor: accent,
    cursorAccent: background,
    selectionForeground: foreground,
    selectionBackground: scheme === "light" ? "rgba(38, 106, 120, 0.22)" : "rgba(130, 200, 207, 0.34)",
    matchBackground: scheme === "light" ? "rgba(217, 146, 0, 0.24)" : "rgba(224, 168, 75, 0.28)",
    activeMatchBackground: scheme === "light" ? "rgba(217, 119, 6, 0.48)" : "rgba(242, 196, 115, 0.55)",
    link: scheme === "light" ? "#0b5cad" : "#79a6dc",
  };
}

function filled<T extends object>(values: T): Partial<T> {
  return Object.fromEntries(
    Object.entries(values).filter(([, value]) => typeof value === "string" && value.trim() !== ""),
  ) as Partial<T>;
}

/** Convert `#rgb` or `#rrggbb` to 0xRRGGBB for libghostty WASM config. */
export function hexColorToInt(color: string): number {
  const trimmed = color.trim();
  if (!trimmed.startsWith("#")) return 0;
  let hex = trimmed.slice(1);
  if (hex.length === 3) hex = hex[0] + hex[0] + hex[1] + hex[1] + hex[2] + hex[2];
  const parsed = Number.parseInt(hex, 16);
  return Number.isNaN(parsed) ? 0 : parsed;
}

export function toWasmTerminalConfig(colors: TerminalColors, scrollbackLimit = 10_000) {
  return {
    scrollbackLimit,
    fgColor: hexColorToInt(colors.foreground),
    bgColor: hexColorToInt(colors.background),
    cursorColor: hexColorToInt(colors.cursor),
    palette: [
      colors.black, colors.red, colors.green, colors.yellow,
      colors.blue, colors.magenta, colors.cyan, colors.white,
      colors.brightBlack, colors.brightRed, colors.brightGreen, colors.brightYellow,
      colors.brightBlue, colors.brightMagenta, colors.brightCyan, colors.brightWhite,
    ].map(hexColorToInt),
  };
}

// ------------------------------------------------------------------ contrast

function luminance(hex: string): number {
  const value = hex.replace("#", "");
  const channels = [0, 2, 4].map((i) => parseInt(value.slice(i, i + 2), 16) / 255);
  const [r, g, b] = channels.map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG contrast ratio between two #rrggbb colours (1–21). */
export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

// ------------------------------------------------------------ font preference

export const TERMINAL_FONT =
  '"JetBrains Mono", "SF Mono", "Cascadia Mono", Consolas, "Liberation Mono", ui-monospace, "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", "Noto Sans Mono CJK SC", monospace';
export const TERMINAL_FONT_DEFAULT = 14;
export const TERMINAL_FONT_MIN = 10;
export const TERMINAL_FONT_MAX = 24;
export const TERMINAL_LINE_HEIGHT = 1.25;
export const TERMINAL_SCROLLBACK_LIMIT = 10_000;

const FONT_SIZE_KEY = "cabin.terminalFontSize";
const fontListeners = new Set<() => void>();
let currentFontSize: number | null = null;

export function clampTerminalFontSize(size: number): number {
  if (!Number.isFinite(size)) return TERMINAL_FONT_DEFAULT;
  return Math.max(TERMINAL_FONT_MIN, Math.min(TERMINAL_FONT_MAX, Math.round(size)));
}

export function getTerminalFontSize(): number {
  if (currentFontSize !== null) return currentFontSize;
  try {
    const raw = typeof localStorage !== "undefined" ? localStorage.getItem(FONT_SIZE_KEY) : null;
    currentFontSize = raw == null ? TERMINAL_FONT_DEFAULT : clampTerminalFontSize(Number(raw));
  } catch {
    currentFontSize = TERMINAL_FONT_DEFAULT;
  }
  return currentFontSize;
}

export function setTerminalFontSize(size: number): number {
  const next = clampTerminalFontSize(size);
  if (next === getTerminalFontSize()) return next;
  currentFontSize = next;
  try {
    if (typeof localStorage !== "undefined") localStorage.setItem(FONT_SIZE_KEY, String(next));
  } catch {
    // Keep the in-memory preference when storage is unavailable.
  }
  fontListeners.forEach((listener) => listener());
  return next;
}

/** Change font size by `delta` (`0` resets to default). */
export function stepTerminalFontSize(delta: number): number {
  return setTerminalFontSize(delta === 0 ? TERMINAL_FONT_DEFAULT : getTerminalFontSize() + delta);
}

export function subscribeTerminalFontSize(listener: () => void): () => void {
  fontListeners.add(listener);
  return () => { fontListeners.delete(listener); };
}

let fontsReady: Promise<void> | undefined;

/** Ensure the bundled monospace faces are ready before measuring canvas metrics. */
export function loadTerminalFonts(): Promise<void> {
  if (typeof document === "undefined" || !document.fonts) return Promise.resolve();
  fontsReady ??= Promise.allSettled(
    ["400", "700", "italic 400", "italic 700"].map((style) =>
      document.fonts.load(`${style} ${TERMINAL_FONT_DEFAULT}px "JetBrains Mono"`, "M0@─█"),
    ),
  ).then(() => undefined);
  return fontsReady;
}
