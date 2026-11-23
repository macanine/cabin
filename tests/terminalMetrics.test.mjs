import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { Ghostty } from "ghostty-web";
import { resolveConfig } from "vite";
import { captureViewport, restoreViewport } from "../src/lib/terminalViewport.ts";
import { paintTerminalGlyph } from "../src/lib/terminalGlyphs.ts";

test("TUI rules join across cell edges at fractional scale", () => {
  const rects = [];
  const ctx = { fillRect: (...rect) => rects.push(rect) };
  const scale = 1.5;
  assert.equal(paintTerminalGlyph(ctx, 0x250c, 0, 0, 9, 17, scale), true);
  assert.equal(paintTerminalGlyph(ctx, 0x2500, 9, 0, 9, 17, scale), true);
  assert.equal(paintTerminalGlyph(ctx, 0x2502, 0, 17, 9, 17, scale), true);
  const [cornerVertical, cornerHorizontal, horizontal, verticalTop] = rects;
  assert.equal(cornerHorizontal[0] + cornerHorizontal[2], horizontal[0]);
  assert.equal(cornerVertical[1] + cornerVertical[3], verticalTop[1]);
  assert.equal(paintTerminalGlyph(ctx, 0x4f60, 0, 0, 9, 17, scale), false);
});

test("Ghostty font metrics are patched for versioned dev module URLs", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const config = await resolveConfig({ root, configFile: join(root, "vite.config.ts"), logLevel: "silent" }, "serve");
  const plugin = config.plugins.find((item) => item.name === "ghostty-cell-metrics");
  assert.ok(plugin && typeof plugin.transform === "function");
  const path = join(root, "node_modules/ghostty-web/dist/ghostty-web.js");
  const source = await readFile(path, "utf8");
  for (const suffix of ["", "?v=dev-test"]) {
    const code = await plugin.transform.call({}, source, `${path}${suffix}`);
    assert.equal(typeof code, "string");
    assert.match(code, /fontBoundingBoxAscent\s*\|\|\s*\w+\.actualBoundingBoxAscent/);
    assert.match(code, /fontBoundingBoxDescent\s*\|\|\s*\w+\.actualBoundingBoxDescent/);
  }
});

test("Ghostty restores the visible line when a full buffer evicts rows without changing length", async () => {
  const wasm = await readFile(new URL(import.meta.resolve("ghostty-web/ghostty-vt.wasm")));
  const ghostty = await Ghostty.load(`data:application/wasm;base64,${wasm.toString("base64")}`);
  const terminal = ghostty.createTerminal(80, 24, { scrollbackLimit: 10000 });
  const lines = (start, count) => Array.from({ length: count }, (_, i) => `row ${start + i}\r\n`).join("");
  const line = (index) => terminal.getScrollbackLine(index)?.map((cell) => String.fromCodePoint(cell.codepoint || 32)).join("").trimEnd();
  try {
    terminal.write("\x1bc" + lines(0, 5000));
    const before = terminal.getScrollbackLength();
    const anchor = captureViewport(before, 80, line);
    assert.ok(anchor);
    terminal.write(lines(5000, 589));
    const after = terminal.getScrollbackLength();
    assert.equal(after, before, "the same number of rows were appended and evicted");
    assert.notEqual(line(anchor.index), anchor.rows[0], "the original index now contains another row");
    const restored = restoreViewport(anchor, after, line);
    assert.equal(line(after - restored), anchor.rows[0]);

    terminal.write(lines(5589, 5000));
    const trimmed = terminal.getScrollbackLength();
    assert.equal(restoreViewport(anchor, trimmed, line), trimmed, "an evicted anchor falls back to the oldest row");
  } finally {
    terminal.free();
  }
});
