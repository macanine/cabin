import assert from "node:assert/strict";
import test from "node:test";
import { matchDisplay, zoomScale } from "../src/lib/chromeDisplay.ts";

test("matching a Retina window bounds the framebuffer and preserves its aspect ratio", () => {
  assert.deepEqual(matchDisplay(1440, 900, 2), { width: 1920, height: 1200, pixelRatio: 4 / 3 });
  assert.deepEqual(matchDisplay(1300, 700, 1), { width: 1300, height: 700, pixelRatio: 1 });
  assert.deepEqual(matchDisplay(800, 600, 2), { width: 1600, height: 1200, pixelRatio: 2 });
});

test("zoom applies to the given compositor scale in fractional scale increments", () => {
  assert.equal(zoomScale(2, 125), 2.5);
  assert.equal(zoomScale(4 / 3, 125), 200 / 120);
  assert.equal(zoomScale(0.75, 100), 1);
});
