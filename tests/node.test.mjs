import assert from "node:assert/strict";
import test from "node:test";
import { archiveUrl, majorOf, recommendedRelease, releaseLines } from "../src/lib/nodeVersions.ts";
import { cellAt, WheelAccumulator, wheelReport } from "../src/lib/terminalInput.ts";

const release = (version, lts = null, installed = false) => ({
  version, major: Number(version.slice(1).split(".")[0]), lts, date: "2026-11-01", npm: null, security: false, installed,
});

test("release lines are classified and the Active LTS is recommended", () => {
  const releases = [
    release("v27.2.0"), release("v26.1.0", "Ninja"), release("v26.0.0"), release("v25.9.0"),
    release("v24.11.0", "Krypton", true), release("v22.20.0", "Jod"), release("v20.19.0", "Iron"), release("v18.20.8", "Hydrogen"),
  ];
  const lines = releaseLines(releases, 22);
  assert.deepEqual(lines.map((l) => [l.major, l.status]), [
    [27, "current"], [26, "activeLts"], [25, "endOfLife"], [24, "maintenanceLts"],
    [22, "maintenanceLts"], [20, "endOfLife"], [18, "endOfLife"],
  ]);
  assert.equal(lines[1].codename, "Ninja");
  assert.equal(lines[1].releases.length, 2);
  assert.equal(lines[3].installed, true);
  assert.equal(recommendedRelease(lines).version, "v26.1.0");
});

test("archive URLs and majors", () => {
  assert.equal(archiveUrl("https://nodejs.org/dist/", "v26.1.0", "linux-arm64"),
    "https://nodejs.org/dist/v26.1.0/node-v26.1.0-linux-arm64.tar.gz");
  assert.equal(majorOf("v18.19.1"), 18);
  assert.equal(majorOf("nope"), null);
});

test("wheel reports use SGR when asked and X10 otherwise", () => {
  assert.equal(wheelReport(64, 3, 7, true), "\x1b[<64;3;7M");
  assert.equal(wheelReport(65, 1, 1, false), "\x1b[M\x61\x21\x21");
  assert.equal(wheelReport(65, 500, 1, false).charCodeAt(4), 32 + 223);
});

test("wheel deltas accumulate into notches and reset on direction change", () => {
  const wheel = new WheelAccumulator();
  assert.equal(wheel.take(8, 0, 16, 30), 0);
  assert.equal(wheel.take(8, 0, 16, 30), 1);
  assert.equal(wheel.take(-3, 1, 16, 30), -3);
  assert.equal(wheel.take(1, 2, 16, 30), 10);
});

test("cellAt maps the pointer to a clamped 1-based cell", () => {
  const rect = { left: 10, top: 20, width: 800, height: 600 };
  assert.deepEqual(cellAt(10, 20, rect, 100, 30), { col: 1, row: 1 });
  assert.deepEqual(cellAt(809, 619, rect, 100, 30), { col: 100, row: 30 });
  assert.deepEqual(cellAt(5000, -5, rect, 100, 30), { col: 100, row: 1 });
});

import { focusPhase, phaseElapsed, reportElapsed } from "../src/lib/startReport.ts";
import { formatDuration } from "../src/lib/format.ts";

test("start report timing", () => {
  assert.equal(formatDuration(3240), "3.2s");
  assert.equal(formatDuration(1000), "1s");
  assert.equal(formatDuration(1020), "1s");
  assert.equal(formatDuration(0), "0s");
  assert.equal(formatDuration(9960), "10s");
  assert.equal(formatDuration(42_400), "42s");
  assert.equal(formatDuration(65_000), "1m 05s");
  assert.equal(formatDuration(3_780_000), "1h 3m");
  const phase = (state, startedAt, finishedAt) => ({ id: state, title: state, state, startedAt, finishedAt, detail: null });
  assert.equal(phaseElapsed(phase("running", 1000, null), 4000), 3000);
  assert.equal(phaseElapsed(phase("done", 1000, 2500), 9999), 1500);
  assert.equal(phaseElapsed(phase("pending", null, null), 9999), null);
  const report = {
    state: "running", startedAt: 1000, finishedAt: null, error: null, facts: [], logPath: null, logTail: [],
    serialPath: null, serialTail: [], phases: [phase("done", 1000, 2000), phase("running", 2000, null), phase("pending", null, null)],
  };
  assert.equal(reportElapsed(report, 5000), 4000);
  assert.equal(focusPhase(report).state, "running");
});
