import assert from "node:assert/strict";
import test from "node:test";
import {
  changeCount,
  changeGroups,
  fileGlyph,
  formatBytes,
  fuzzyFilter,
  gitDecorations,
  languageFor,
  parsePreviewTarget,
  previewAddress,
  statusLetter,
} from "../src/lib/workbench.ts";

const file = (path, status, extra = {}) => ({ path, origPath: null, status, additions: null, deletions: null, ...extra });
const status = (parts = {}) => ({
  repository: true, branch: "main", upstream: null, ahead: 0, behind: 0, head: "abc1234",
  conflicted: [], staged: [], unstaged: [], untracked: [], truncated: false, ...parts,
});

test("languages and glyphs follow the file name", () => {
  assert.equal(languageFor("src/App.tsx"), "tsx");
  assert.equal(languageFor("core/src/lib.rs"), "rust");
  assert.equal(languageFor("Dockerfile"), "docker");
  assert.equal(languageFor(".env.local"), "dotenv");
  assert.equal(languageFor("LICENSE"), "text");
  assert.equal(fileGlyph("README.md"), "markdown");
  assert.equal(fileGlyph("logo.svg"), "image");
  assert.equal(fileGlyph("Cargo.lock"), "lock");
  assert.equal(fileGlyph(".gitignore"), "git");
});

test("untracked files join the changes group, sorted", () => {
  const groups = changeGroups(status({
    staged: [file("b.ts", "M")],
    unstaged: [file("z.ts", "M"), file("b.ts", "M")],
    untracked: [file("a.ts", "?")],
  }));
  assert.deepEqual(groups.changes.map((f) => f.path), ["a.ts", "b.ts", "z.ts"]);
  assert.equal(groups.staged.length, 1);
  // b.ts is staged and changed again: one path in the badge.
  assert.equal(changeCount(status({ staged: [file("b.ts", "M")], unstaged: [file("b.ts", "M")] })), 1);
  assert.equal(changeCount(null), 0);
  assert.deepEqual(changeGroups({ ...status(), repository: false }).changes, []);
});

test("folders take the strongest change below them", () => {
  const map = gitDecorations(status({
    untracked: [file("src/new/x.ts", "?")],
    unstaged: [file("src/lib/a.ts", "M")],
    conflicted: [file("src/lib/c.ts", "U")],
  }));
  assert.equal(map.get("src/new/x.ts"), "untracked");
  assert.equal(map.get("src/new"), "untracked");
  assert.equal(map.get("src/lib"), "conflict");
  assert.equal(map.get("src"), "conflict");
  assert.equal(statusLetter(file("a", "?")), "U");
  assert.equal(statusLetter(file("a", "U")), "!");
});

test("preview targets stay on the VM's localhost", () => {
  assert.deepEqual(parsePreviewTarget("3000", null), { port: 3000, path: "/" });
  assert.deepEqual(parsePreviewTarget(":5173/docs", null), { port: 5173, path: "/docs" });
  assert.deepEqual(parsePreviewTarget("localhost:8080/a?b=1#c", null), { port: 8080, path: "/a?b=1#c" });
  assert.deepEqual(parsePreviewTarget("http://127.0.0.1:4000", null), { port: 4000, path: "/" });
  assert.deepEqual(parsePreviewTarget("http://[::1]:4000/x", null), { port: 4000, path: "/x" });
  assert.deepEqual(parsePreviewTarget("/settings", 3000), { port: 3000, path: "/settings" });
  assert.equal(parsePreviewTarget("/settings", null), null);
  assert.equal(parsePreviewTarget("https://example.com", null), null);
  assert.equal(parsePreviewTarget("99999", null), null);
  assert.equal(parsePreviewTarget("file:///etc/passwd", null), null);
  assert.equal(previewAddress({ port: 3000, path: "/" }), "localhost:3000");
  assert.equal(previewAddress({ port: 3000, path: "/a" }), "localhost:3000/a");
});

test("fuzzy filter ranks file-name runs first", () => {
  const paths = ["src/components/Chat.tsx", "src/lib/chatTimeline.ts", "docs/c-h-a-t.md", "README.md"];
  const ranked = fuzzyFilter("chat", paths);
  assert.equal(ranked[0], "src/components/Chat.tsx");
  assert.ok(!ranked.includes("README.md"));
  assert.deepEqual(fuzzyFilter("", paths), paths);
});

test("byte sizes are English", () => {
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(2048), "2.0 KB");
  assert.equal(formatBytes(3 * 1024 * 1024), "3.0 MB");
});
