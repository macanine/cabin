import assert from "node:assert/strict";
import test from "node:test";
import { ANSI, BASE, contrast, terminalColors } from "../src/lib/terminalTheme.ts";

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
