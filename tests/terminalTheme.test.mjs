import assert from "node:assert/strict";
import test from "node:test";
import {
  ANSI, BASE, clampTerminalFontSize, contrast, getTerminalFontSize,
  setTerminalFontSize, stepTerminalFontSize, subscribeTerminalFontSize,
  TERMINAL_FONT, TERMINAL_FONT_DEFAULT, TERMINAL_FONT_MAX, TERMINAL_FONT_MIN,
  terminalColors, toWasmTerminalConfig,
} from "../src/lib/terminal/theme.ts";

for (const scheme of ["light", "dark"]) {
  test(`${scheme}: body text is comfortably readable (WCAG AAA)`, () => {
    const { background, foreground } = BASE[scheme];
    assert.ok(contrast(foreground, background) >= 7, `${foreground} on ${background}`);
  });

  test(`${scheme}: every ANSI colour used for text stands out from the background`, () => {
    const { background } = BASE[scheme];
    // The colour that matches the background family is a fill colour, not text.
    const fill = scheme === "dark" ? "black" : null;
    for (const [name, color] of Object.entries(ANSI[scheme])) {
      if (name === fill) continue;
      const ratio = contrast(color, background);
      assert.ok(ratio >= 3, `${scheme} ${name} ${color}: ${ratio.toFixed(2)}`);
    }
  });
}

test("CSS tokens override the defaults; blanks are ignored", () => {
  const colors = terminalColors("dark", { background: "#000000", foreground: "  ", accent: "" });
  assert.equal(colors.background, "#000000");
  assert.equal(colors.foreground, BASE.dark.foreground);
  assert.equal(colors.cursor, BASE.dark.accent);
  assert.equal(colors.red, ANSI.dark.red);
});

test("WASM terminal config encodes palette and base colors cleanly", () => {
  const config = toWasmTerminalConfig(terminalColors("dark"), 5000);
  assert.equal(config.scrollbackLimit, 5000);
  assert.equal(config.bgColor, 0x171b21);
  assert.equal(config.fgColor, 0xe0e8ef);
  assert.equal(config.palette.length, 16);
});

test("terminal font size clamps, steps, resets, and notifies subscribers", () => {
  assert.equal(clampTerminalFontSize(NaN), TERMINAL_FONT_DEFAULT);
  assert.equal(clampTerminalFontSize(4), TERMINAL_FONT_MIN);
  assert.equal(clampTerminalFontSize(99), TERMINAL_FONT_MAX);

  let notifications = 0;
  const off = subscribeTerminalFontSize(() => { notifications += 1; });
  try {
    setTerminalFontSize(TERMINAL_FONT_DEFAULT);
    const before = notifications;
    assert.equal(stepTerminalFontSize(2), TERMINAL_FONT_DEFAULT + 2);
    assert.equal(getTerminalFontSize(), TERMINAL_FONT_DEFAULT + 2);
    assert.equal(notifications, before + 1);
    assert.equal(stepTerminalFontSize(0), TERMINAL_FONT_DEFAULT);
    assert.equal(notifications, before + 2);
  } finally {
    off();
    setTerminalFontSize(TERMINAL_FONT_DEFAULT);
  }
});

test("terminal font stack checks monospace fallbacks before CJK fonts", () => {
  const families = TERMINAL_FONT.split(",").map((name) => name.trim().replace(/^"|"$/g, ""));
  const firstCjk = Math.min(
    ...["PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", "Noto Sans Mono CJK SC"].map((name) =>
      families.indexOf(name),
    ),
  );
  assert.ok(firstCjk > 0, "CJK fallbacks should be present");
  for (const mono of ["JetBrains Mono", "SF Mono", "Menlo", "Cascadia Mono", "Consolas", "Liberation Mono"]) {
    const idx = families.indexOf(mono);
    assert.ok(idx >= 0 && idx < firstCjk, `${mono} (${idx}) must precede CJK fallbacks (${firstCjk})`);
  }
});
