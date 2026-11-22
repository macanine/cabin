import assert from "node:assert/strict";
import test from "node:test";
import { environmentSummary, restartsChrome, settingsChanged, settingsProblem } from "../src/lib/browserEnvironment.ts";

const settings = {
  followMachine: false, timezone: "Asia/Tokyo", locale: "ja-JP", userAgent: "Mozilla/5.0 Test",
  platform: "Win32", hardwareConcurrency: 8, deviceMemory: 8,
};
const state = { timezone: "Asia/Tokyo", language: "ja-JP", languages: ["ja-JP", "ja"], platform: "Win32" };
const view = (probe, liveMatches = null) => ({
  enabled: true, machineRunning: true, settings, liveMatches, checkedAt: 1,
  probe: probe && { enabled: true, expected: state, installed: state, observed: null, liveDetail: null, error: null, ...probe },
});

test("a closed or sleeping Chrome is fine, not a problem", () => {
  assert.equal(environmentSummary(view({ live: "notRunning" })).tone, "ok");
  const asleep = environmentSummary(view({ live: "idle", liveDetail: "Chrome's pages are asleep" }));
  assert.equal(asleep.tone, "ok");
  assert.match(asleep.detail, /asleep/);
  assert.equal(asleep.action, null);
});

test("an open Chrome on older settings blocks and asks for a restart; so do wrong installed settings", () => {
  const stale = environmentSummary(view({ live: "observed", observed: { ...state, timezone: "UTC" } }, false));
  assert.deepEqual([stale.tone, stale.action], ["bad", "restart"]);
  const broken = environmentSummary(view({ live: "notRunning", error: "The installed browser settings differ" }));
  assert.deepEqual([broken.tone, broken.action], ["bad", "check"]);
  assert.equal(environmentSummary(view({ live: "observed", observed: state }, true)).tone, "ok");
});

test("states without a check", () => {
  assert.equal(environmentSummary(null).tone, "busy");
  assert.equal(environmentSummary({ ...view(null), enabled: false }).title, "No managed browser");
  assert.equal(environmentSummary({ ...view(null), machineRunning: false }).tone, "muted");
  assert.equal(environmentSummary(view(null)).action, "check");
});

test("only time zone and language apply without restarting Chrome", () => {
  assert.equal(restartsChrome(settings, { ...settings, timezone: "Europe/Berlin", locale: "de-DE" }), false);
  assert.equal(restartsChrome(settings, { ...settings, userAgent: "Other" }), true);
  assert.equal(restartsChrome(settings, { ...settings, followMachine: true }), true);
  assert.equal(settingsChanged(settings, { ...settings }), false);
  assert.equal(settingsChanged(settings, { ...settings, deviceMemory: 4 }), true);
});

test("drafts are validated before saving", () => {
  assert.equal(settingsProblem(settings), null);
  assert.ok(settingsProblem({ ...settings, userAgent: "a\nb" }));
  assert.ok(settingsProblem({ ...settings, timezone: "Europe/Berlin; x" }));
  assert.ok(settingsProblem({ ...settings, deviceMemory: 3 }));
  assert.ok(settingsProblem({ ...settings, hardwareConcurrency: 0 }));
  // Following the machine: region and hardware come from it.
  assert.equal(settingsProblem({ ...settings, followMachine: true, timezone: "", deviceMemory: 3 }), null);
});
