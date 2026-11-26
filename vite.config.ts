import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import type { Plugin } from "vite";
import { TERMINAL_LINE_HEIGHT, TERMINAL_SCROLLBAR_GUTTER } from "./src/lib/terminalAppearance";
// @ts-expect-error type error without @types/node package
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

/**
 * ghostty-web 0.4.0 sizes terminal rows from the bounding box of "M", so
 * i-dots, accents and descenders spill out of their row (the top row gets
 * clipped, lines look cramped). Use the font's full ascent/descent instead.
 * Fails the build if the upstream code changes, so the patch is revisited.
 */
function ghosttyCellMetrics(): Plugin {
  // The module embeds WASM as a long base64 string. Start only at identifier
  // boundaries to avoid quadratic backtracking while scanning that payload.
  const ascent = /(?<![\w$])([\w$]+)\.actualBoundingBoxAscent \|\| this\.fontSize \* 0\.8/;
  const rows = /(?<![\w$])([\w$]+) = Math\.ceil\(([\w$]+) \+ ([\w$]+)\) \+ 2, ([\w$]+) = Math\.ceil\(\2\) \+ 1/;
  const descent = /(?<![\w$])([\w$]+)\.actualBoundingBoxDescent \|\| this\.fontSize \* 0\.2/;
  const canvasWidth = /const ([\w$]+) = ([\w$]+) \* this\.metrics\.width, ([\w$]+) = ([\w$]+) \* this\.metrics\.height/;
  const backingSize = /this\.canvas\.width = ([\w$]+) \* this\.devicePixelRatio, this\.canvas\.height = ([\w$]+) \* this\.devicePixelRatio/;
  const needsResize = /this\.canvas\.width !== ([\w$]+)\.cols \* this\.metrics\.width \* this\.devicePixelRatio \|\| this\.canvas\.height !== \1\.rows \* this\.metrics\.height \* this\.devicePixelRatio/;
  const glyphPaint = /(String\.fromCodePoint\(([\w$]+)\.codepoint \|\| 32\), )this\.ctx\.fillText\(([\w$]+), ([\w$]+), ([\w$]+)\)/;
  const duplicateSizing = / {4}const ([\w$]+) = this\.renderer\.getMetrics\(\);\n {4}this\.canvas\.width = \1\.width \* ([\w$.]+), this\.canvas\.height = \1\.height \* ([\w$.]+), this\.canvas\.style\.width = `\$\{\1\.width \* \2\}px`, this\.canvas\.style\.height = `\$\{\1\.height \* \3\}px`, /g;
  return {
    name: "ghostty-cell-metrics",
    transform(code, id) {
      if (!/ghostty-web[\\/]dist[\\/]ghostty-web\.js$/.test(id.split("?", 1)[0])) return null;
      if (![ascent, descent, rows, canvasWidth, backingSize, needsResize, glyphPaint].every((pattern) => pattern.test(code)) ||
          [...code.matchAll(duplicateSizing)].length !== 2) {
        throw new Error("ghostty-web canvas code changed; review the ghostty-cell-metrics patch in vite.config.ts");
      }
      const patched = code
        .replace(ascent, "($1.fontBoundingBoxAscent || $1.actualBoundingBoxAscent || this.fontSize * 0.8)")
        .replace(descent, "($1.fontBoundingBoxDescent || $1.actualBoundingBoxDescent || this.fontSize * 0.2)")
        .replace(rows, `$1 = Math.max(Math.ceil($2 + $3), Math.round(this.fontSize * ${TERMINAL_LINE_HEIGHT})), $4 = Math.round($2 + ($1 - $2 - $3) / 2)`)
        // Give the built-in scrollbar its own strip so it never obscures cells.
        .replace(canvasWidth, `const $1 = $2 * this.metrics.width + ${TERMINAL_SCROLLBAR_GUTTER}, $3 = $4 * this.metrics.height`)
        .replace(backingSize, "this.canvas.width = Math.round($1 * this.devicePixelRatio), this.canvas.height = Math.round($2 * this.devicePixelRatio)")
        .replace(needsResize, `(this.devicePixelRatio !== (window.devicePixelRatio || 1) && (this.devicePixelRatio = window.devicePixelRatio || 1, true)) || this.canvas.width !== Math.round(($1.cols * this.metrics.width + ${TERMINAL_SCROLLBAR_GUTTER}) * this.devicePixelRatio) || this.canvas.height !== Math.round($1.rows * this.metrics.height * this.devicePixelRatio)`)
        // Let CanvasRenderer own backing dimensions and context scaling.
        .replace(duplicateSizing, "    ")
        .replace(glyphPaint, "$1paintTerminalGlyph(this.ctx, $2.codepoint, $4, $5 - this.metrics.baseline, this.metrics.width * $2.width, this.metrics.height, this.devicePixelRatio) || this.ctx.fillText($3, $4, $5)");
      return `import { paintTerminalGlyph } from "/src/lib/terminalGlyphs.ts";\n${patched}`;
    },
  };
}

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [tailwindcss(), react(), ghosttyCellMetrics()],
  // A source tree can be shared with a Linux VM; avoid sharing Vite's cache.
  cacheDir: `node_modules/.vite-${process.platform}-${process.arch}`,
  // Keep ghostty-web out of dep pre-bundling so the patch above applies in dev too.
  optimizeDeps: { exclude: ["ghostty-web"] },
  // A desktop bundle loads from disk; one chunk is fine.
  build: { chunkSizeWarningLimit: 1500 },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
