/**
 * Terminal colours that follow the app's light/dark scheme. Background and
 * foreground match the content card (`--terminal-bg` / `--terminal-fg` in
 * styles/base.css), so the terminal reads as part of the page rather than a black
 * box; the 16 ANSI colours are tuned per scheme so CLI output (prompts, diffs,
 * agent UIs) stays legible on either background. `tests/terminalTheme.test.mjs`
 * checks the contrast.
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
}

/** Fallbacks matching styles/base.css, used when the CSS variables are unavailable. */
export const BASE: Record<ColorScheme, { background: string; foreground: string; accent: string }> = {
  light: { background: "#fbfbfa", foreground: "#1d1c1a", accent: "#c2622d" },
  dark: { background: "#1c1b19", foreground: "#ecebe8", accent: "#e0814b" },
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

/** Full terminal theme for a scheme; `base` overrides come from CSS tokens. */
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
    selectionBackground: scheme === "light" ? "rgba(194, 98, 45, 0.22)" : "rgba(224, 129, 75, 0.35)",
  };
}

function filled<T extends object>(values: T): Partial<T> {
  return Object.fromEntries(Object.entries(values).filter(([, value]) => typeof value === "string" && value.trim() !== "")) as Partial<T>;
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
