import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { Ghostty } from "ghostty-web";
import {
  captureViewport, encodeKeyEvent, mouseReport, restoreViewport,
} from "../src/lib/terminal/protocol.ts";
import { paintTerminalGlyph } from "../src/lib/terminal/renderer.ts";
import {
  extractSelectionText, findLineBounds, findUrlAt, findWordBounds, searchTerminalBuffer,
} from "../src/lib/terminal/buffer.ts";

let cachedGhostty;
async function getGhostty() {
  if (!cachedGhostty) {
    const wasm = await readFile(new URL(import.meta.resolve("ghostty-web/ghostty-vt.wasm")));
    cachedGhostty = await Ghostty.load(`data:application/wasm;base64,${wasm.toString("base64")}`);
  }
  return cachedGhostty;
}

const key = (overrides) => ({
  key: "", code: "", ctrlKey: false, altKey: false, shiftKey: false, metaKey: false,
  ...overrides,
});

test("TUI rules, rounded corners, and shades join cleanly at fractional scale", () => {
  const rects = [];
  const ctx = { globalAlpha: 1, fillRect: (...rect) => rects.push(rect) };
  const scale = 1.5;
  assert.equal(paintTerminalGlyph(ctx, 0x250c, 0, 0, 9, 17, scale), true);
  assert.equal(paintTerminalGlyph(ctx, 0x2500, 9, 0, 9, 17, scale), true);
  assert.equal(paintTerminalGlyph(ctx, 0x2502, 0, 17, 9, 17, scale), true);
  const [cornerVertical, cornerHorizontal, horizontal, verticalTop] = rects;
  assert.equal(cornerHorizontal[0] + cornerHorizontal[2], horizontal[0]);
  assert.equal(cornerVertical[1] + cornerVertical[3], verticalTop[1]);
  assert.equal(paintTerminalGlyph(ctx, 0x256d, 0, 0, 9, 17, scale), true);
  assert.equal(paintTerminalGlyph(ctx, 0x2592, 0, 0, 9, 17, scale), true);
  assert.equal(ctx.globalAlpha, 1);
  assert.equal(paintTerminalGlyph(ctx, 0x4f60, 0, 0, 9, 17, scale), false);
});

test("keyboard encoder handles Shift+Tab, DECCKM arrows, modifiers, Ctrl, and macOS shortcuts", () => {
  const mac = { appCursor: false, isMac: true, altScreen: false };
  const macApp = { appCursor: true, isMac: true, altScreen: true };
  const win = { appCursor: false, isMac: false, altScreen: false };

  assert.equal(encodeKeyEvent(key({ key: "Tab", shiftKey: true }), mac), "\x1b[Z");
  assert.equal(encodeKeyEvent(key({ key: "Tab" }), mac), "\t");
  assert.equal(encodeKeyEvent(key({ key: "ArrowUp" }), mac), "\x1b[A");
  assert.equal(encodeKeyEvent(key({ key: "ArrowUp" }), macApp), "\x1bOA");
  assert.equal(encodeKeyEvent(key({ key: "ArrowRight", ctrlKey: true }), mac), "\x1b[1;5C");
  assert.equal(encodeKeyEvent(key({ key: "ArrowLeft", altKey: true }), mac), "\x1bb");
  assert.equal(encodeKeyEvent(key({ key: "ArrowLeft", altKey: true }), macApp), "\x1b[1;3D");
  assert.equal(encodeKeyEvent(key({ key: "Backspace", metaKey: true }), mac), "\x15");
  assert.equal(encodeKeyEvent(key({ key: "Backspace", metaKey: true }), macApp), null);
  assert.equal(encodeKeyEvent(key({ key: "c", ctrlKey: true }), win), "\x03");
  assert.equal(encodeKeyEvent(key({ key: " ", ctrlKey: true, code: "Space" }), win), "\x00");
  assert.equal(encodeKeyEvent(key({ key: "@", ctrlKey: true, altKey: true }), win), "@");
  assert.equal(encodeKeyEvent(key({ key: "é", altKey: true }), mac), "é");
  assert.equal(encodeKeyEvent(key({ key: "b", altKey: true }), mac), "\x1bb");
  assert.equal(encodeKeyEvent(key({ key: "Enter", altKey: true }), mac), "\x1b\r");
});

test("mouse reports encode SGR press, release, motion, and modifiers accurately", () => {
  assert.equal(mouseReport(0, 12, 4, { sgr: true }), "\x1b[<0;12;4M");
  assert.equal(mouseReport(0, 12, 4, { sgr: true, release: true }), "\x1b[<0;12;4m");
  assert.equal(mouseReport(0, 12, 4, { sgr: true, motion: true, ctrl: true }), "\x1b[<48;12;4M");
  assert.equal(mouseReport(0, 5, 5, { sgr: false, release: true }), "\x1b[M\x23\x25\x25");
});

test("Ghostty restores the visible line when a full buffer evicts rows without changing length", async () => {
  const ghostty = await getGhostty();
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

test("buffer selection, word bounds, search, and URL detection work across scrollback and CJK text", async () => {
  const ghostty = await getGhostty();
  const terminal = ghostty.createTerminal(40, 6, { scrollbackLimit: 500 });
  try {
    terminal.write("\x1bc");
    terminal.write("old-token https://example.com/docs(1).\r\n");
    terminal.write("你好世界 hello_world\r\n");
    for (let i = 0; i < 8; i++) terminal.write(`line ${i}\r\n`);
    terminal.write("active-token needle\r\n");
    terminal.update();

    const scrollback = terminal.getScrollbackLength();
    assert.ok(scrollback >= 4, "first lines moved into scrollback");

    // Word selection works inside scrollback (ghostty-web's SelectionManager broke here).
    const word = findWordBounds(terminal, 0, 2);
    assert.deepEqual(word, { startCol: 0, endCol: 8 });
    assert.equal(
      extractSelectionText(terminal, { absRow: 0, col: word.startCol }, { absRow: 0, col: word.endCol }),
      "old-token",
    );

    // URL detection strips trailing period while keeping balanced parens.
    const url = findUrlAt(terminal, 0, 15);
    assert.equal(url?.url, "https://example.com/docs(1)");

    // CJK wide-character extraction skips continuation cells cleanly.
    const cjkLine = findLineBounds(terminal, 1);
    assert.equal(extractSelectionText(terminal, cjkLine.start, cjkLine.end), "你好世界 hello_world");

    // Search spans both scrollback and active screen.
    const matches = searchTerminalBuffer(terminal, "TOKEN");
    assert.equal(matches.length, 2);
    assert.equal(matches[0].absRow, 0);
    assert.ok(matches[1].absRow >= scrollback);
    assert.equal(searchTerminalBuffer(terminal, "TOKEN", { caseSensitive: true }).length, 0);
  } finally {
    terminal.free();
  }
});
