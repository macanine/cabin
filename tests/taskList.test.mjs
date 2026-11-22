import assert from "node:assert/strict";
import test from "node:test";
import { shortAge, taskSections } from "../src/lib/taskList.ts";

const DAY = 86_400_000;
const NOW = new Date("2026-11-16T15:00:00").getTime();
const ws = (id, name, extra = {}) => ({ id, name, path: `~/${name}`, proxy: { mode: "inherit" }, modelOverrides: [], archived: false, createdAt: 0, updatedAt: 0, ...extra });
const task = (id, workspaceId, name, at, extra = {}) => ({
  id, workspaceId, name, agent: "claude", archived: false, starred: false, createdAt: at, updatedAt: at, lastStartedAt: null,
  status: { state: "idle" }, ...extra,
});
const workspaces = [ws("p1", "Project One"), ws("p2", "Project Two"), ws("p3", "Old", { archived: true }), ws("p4", "Empty")];
const sessions = [
  task("a", "p1", "Fix login", NOW - 2 * 3600_000),
  task("b", "p2", "Write docs", NOW - 3 * DAY),
  task("c", "p1", "Refactor", NOW - 40 * DAY),
  task("d", "p3", "Hidden (archived project)", NOW),
  task("e", "p2", "Hidden (archived task)", NOW, { archived: true }),
];

test("by project: newest project first, empty projects kept, archived hidden", () => {
  const sections = taskSections("project", sessions, workspaces, { now: NOW });
  assert.deepEqual(sections.map((s) => [s.label, s.sessions.map((t) => t.id)]), [
    ["Project One", ["a", "c"]], ["Project Two", ["b"]], ["Empty", []],
  ]);
  assert.equal(sections[0].workspace.id, "p1");
});

test("by session is one flat list, newest conversation first", () => {
  const [all] = taskSections("session", sessions, workspaces, { now: NOW });
  assert.equal(all.label, null);
  assert.deepEqual(all.sessions.map((t) => t.id), ["a", "b", "c"]);
});

test("recently updated uses day buckets; unread splits unread first", () => {
  assert.deepEqual(taskSections("recent", sessions, workspaces, { now: NOW }).map((s) => [s.label, s.sessions.length]), [
    ["Today", 1], ["Previous 7 days", 1], ["Older", 1],
  ]);
  const unread = taskSections("unread", sessions, workspaces, { now: NOW, unread: new Set(["b"]) });
  assert.deepEqual(unread.map((s) => [s.label, s.sessions.map((t) => t.id)]), [["Unread", ["b"]], ["Read", ["a", "c"]]]);
});

test("search matches task and project names; archived can be shown", () => {
  const found = taskSections("project", sessions, workspaces, { query: "docs", now: NOW });
  assert.deepEqual(found.map((s) => s.label), ["Project Two"]);
  const byProject = taskSections("session", sessions, workspaces, { query: "project one", now: NOW });
  assert.deepEqual(byProject[0].sessions.map((t) => t.id), ["a", "c"]);
  const withArchived = taskSections("session", sessions, workspaces, { showArchived: true, now: NOW });
  assert.equal(withArchived[0].sessions.length, 5);
});

test("short ages", () => {
  assert.equal(shortAge(NOW - 20_000, NOW), "now");
  assert.equal(shortAge(NOW - 5 * 60_000, NOW), "5m");
  assert.equal(shortAge(NOW - 3 * 3600_000, NOW), "3h");
  assert.equal(shortAge(NOW - 1 * DAY, NOW), "1 day");
  assert.equal(shortAge(NOW - 3 * DAY, NOW), "3 days");
  assert.equal(shortAge(NOW - 21 * DAY, NOW), "3 wk");
  assert.equal(shortAge(NOW - 90 * DAY, NOW), "3 mo");
});
