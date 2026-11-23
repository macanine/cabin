/**
 * Light and dark console palettes, shared by the canvas and its surrounding
 * surface. Keep ANSI output and the input caret readable in both schemes.
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
    selectionForeground: foreground,
    selectionBackground: scheme === "light" ? "rgba(38, 106, 120, 0.23)" : "rgba(130, 200, 207, 0.36)",
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
