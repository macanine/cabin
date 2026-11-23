import test from "node:test";
import assert from "node:assert/strict";
import {
  defaultGit, defaultRules, folderName, gitWarning, guestPathFor, modeInfo, parsePatterns, slug, syncLine, toggleExcluded,
} from "../src/lib/syncForm.ts";

const status = (fields = {}) => ({ id: "s", state: "watching", error: null, lastSyncAt: 1_000, files: 3, pushed: 0, pulled: 0, conflicts: [], ...fields });

test("git defaults and warnings follow the mode", () => {
  assert.equal(defaultGit("mirror"), "sync");
  assert.equal(defaultGit("twoWay"), "copyOnce");
  assert.equal(defaultRules("safe").git, "copyOnce");
  assert.match(gitWarning("twoWay", "sync"), /break the repository/);
  assert.equal(gitWarning("mirror", "sync"), null);
  assert.equal(gitWarning("twoWay", "copyOnce"), null);
});

test("names, paths and patterns", () => {
  assert.equal(slug("My App!"), "my-app");
  assert.equal(guestPathFor("My App!"), "~/projects/my-app");
  assert.equal(guestPathFor("!!!"), "~/projects/project");
  assert.equal(folderName("/Users/me/code/api/"), "api");
  assert.equal(folderName("/Users/me/code/api\\v2"), "api\\v2");
  assert.equal(folderName("C:\\Users\\me\\code\\api\\"), "api");
  assert.equal(folderName("C:/Users/me/code/api/"), "api");
  assert.equal(folderName("\\\\server\\share\\api\\"), "api");
  assert.deepEqual(parsePatterns("*.log\n\n# note\n build/ "), ["*.log", "build/"]);
  const rules = toggleExcluded(defaultRules(), "docs", false);
  assert.deepEqual(rules.excluded, ["docs"]);
  assert.deepEqual(toggleExcluded(rules, "docs", true).excluded, []);
  assert.equal(modeInfo("twoWay").flow, "Local ⇄ machine");
});

test("one status line, conflicts first", () => {
  assert.deepEqual(syncLine(status({ conflicts: ["a", "b"] })), { tone: "warn", text: "2 conflicts to settle" });
  assert.equal(syncLine(status(), 3_000).text, "In sync · just now");
  assert.equal(syncLine(status(), 91_000).text, "In sync · 2m ago");
  assert.equal(syncLine(status({ state: "offline" })).tone, "off");
  assert.equal(syncLine(status({ state: "error", error: "boom" })).text, "boom");
});
