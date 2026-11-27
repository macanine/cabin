import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
// @ts-expect-error type error without @types/node package
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [tailwindcss(), react()],
  // A source tree can be shared with a Linux VM; avoid sharing Vite's cache.
  cacheDir: `node_modules/.vite-${process.platform}-${process.arch}`,
  // Keep ghostty-web out of dep pre-bundling so its embedded WASM loads cleanly in dev.
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
