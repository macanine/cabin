import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { Ghostty } from "ghostty-web";
import {
  captureViewport, encodeKeyEvent, mouseReport, Osc8Stripper, restoreViewport,
} from "../src/lib/terminal/protocol.ts";
import { atlasSlotBounds, paintTerminalGlyph } from "../src/lib/terminal/renderer.ts";
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

test("atlasSlotBounds expands horizontally for wide fallback symbols and left overhang without changing row height", () => {
  const metrics = { width: 8.5, height: 18, baseline: 14 };
  const dpr = 2;

  const ascii = atlasSlotBounds(metrics, 1, false, dpr);
  assert.equal(ascii.padLeft, 0);
  assert.equal(ascii.h, 36);

  // A 1-cell fallback circle (e.g. ● / ○ from a CJK or symbol font) wider than 1.15 cells must not clip.
  const wideCircle = atlasSlotBounds(metrics, 1, false, dpr, {
    width: 14,
    actualBoundingBoxLeft: -0.5,
    actualBoundingBoxRight: 13.5,
  });
  assert.equal(wideCircle.padLeft, 0);
  assert.equal(wideCircle.h, ascii.h);
  assert.ok(wideCircle.w >= Math.ceil(14 * dpr) + 1, `expected slot width >= 29, got ${wideCircle.w}`);

  // Glyphs with negative left sidebearing (actualBoundingBoxLeft > 0) reserve left padding as well.
  const overhanging = atlasSlotBounds(metrics, 1, false, dpr, {
    width: 9,
    actualBoundingBoxLeft: 2.2,
    actualBoundingBoxRight: 11.4,
  });
  assert.ok(overhanging.padLeft >= Math.ceil(2.2 * dpr) + 1);
  assert.ok(overhanging.w - overhanging.padLeft >= Math.ceil(11.4 * dpr) + 1);
  assert.equal(overhanging.h, ascii.h);
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

test("Osc8Stripper preserves visible text and other escapes across arbitrary splits and prevents Ghostty resize hangs", async () => {
  const sample =
    "\x1b]0;Window Title\x07" +
    "\x1b[34;4m\x1b]8;id=1;file:///home/nic/README.md\x1b\\README.md\x1b]8;;\x1b\\\x1b[0m " +
    "\x1b]8;;https://example.com\x07docs\x1b]8;;\x07 " +
    "\x1b]80;keep-other-osc\x07";
  const expected =
    "\x1b]0;Window Title\x07" +
    "\x1b[34;4mREADME.md\x1b[0m docs " +
    "\x1b]80;keep-other-osc\x07";

  const whole = new Osc8Stripper();
  assert.equal(whole.push(sample), expected);

  // Byte-by-byte streaming across every escape boundary produces identical output.
  const byteWise = new Osc8Stripper();
  let streamed = "";
  for (const ch of sample) streamed += byteWise.push(ch);
  assert.equal(streamed, expected);

  // Feeding stripped OSC 8 wrapped output into Ghostty and shrinking columns never hangs.
  const ghostty = await getGhostty();
  const terminal = ghostty.createTerminal(100, 30, { scrollbackLimit: 10000 });
  const stripper = new Osc8Stripper();
  try {
    terminal.write("\x1bc");
    for (let i = 0; i < 40; i++) {
      const line = `• Project ${i}: According to \x1b]8;;file:///home/nic/README.md\x1b\\README.md\x1b]8;;\x1b\\, it is a Cloudflare Worker with AES-256-GCM encryption.\r\n`;
      terminal.write(stripper.push(line));
    }
    for (let cols = 100; cols >= 40; cols -= 1) {
      terminal.resize(cols, 30);
      terminal.getCursor();
    }
    assert.equal(terminal.getDimensions().cols, 40);
  } finally {
    terminal.free();
  }
});
