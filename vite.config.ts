import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import type { Plugin } from "vite";
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
  // Row height: `renderer.lineHeight` (a factor of the font size, set by
  // components/Terminal.tsx) decides it, never below the font's own box, and
  // the space beyond the glyphs is split evenly above and below.
  const rows = /(?<![\w$])([\w$]+) = Math\.ceil\(([\w$]+) \+ ([\w$]+)\) \+ 2, ([\w$]+) = Math\.ceil\(\2\) \+ 1/;
  const descent = /(?<![\w$])([\w$]+)\.actualBoundingBoxDescent \|\| this\.fontSize \* 0\.2/;
  return {
    name: "ghostty-cell-metrics",
    transform(code, id) {
      if (!/ghostty-web[\\/]dist[\\/]ghostty-web\.js$/.test(id.split("?", 1)[0])) return null;
      if (!ascent.test(code) || !descent.test(code) || !rows.test(code)) {
        throw new Error("ghostty-web font metrics code changed; review the ghostty-cell-metrics patch in vite.config.ts");
      }
      return code
        .replace(ascent, "($1.fontBoundingBoxAscent || $1.actualBoundingBoxAscent || this.fontSize * 0.8)")
        .replace(descent, "($1.fontBoundingBoxDescent || $1.actualBoundingBoxDescent || this.fontSize * 0.2)")
        .replace(rows, "$1 = this.lineHeight ? Math.max(Math.ceil($2 + $3), Math.round(this.fontSize * this.lineHeight)) : Math.ceil($2 + $3) + 2, $4 = Math.ceil($2) + Math.floor(($1 - Math.ceil($2 + $3)) / 2)");
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
