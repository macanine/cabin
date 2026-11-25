import { test } from "node:test";
import assert from "node:assert/strict";
import { applyItem, menuItems, menuKeyAction, menuTrigger } from "../src/lib/composerMenu.ts";
import { sessionCommandFor, sessionCommands } from "../src/lib/sessionCommands.ts";

const cmd = (name, extra = {}) => ({ name, description: null, hint: null, kind: "command", source: null, insert: `/${name} `, ...extra });
const skill = (name, insert, extra = {}) => ({ ...cmd(name), kind: "skill", insert, ...extra });
const file = (path) => ({ ...cmd(path), kind: "file", source: "workspace", insert: `@${path} ` });

test("triggers: /, $ and @ at a token boundary", () => {
  assert.deepEqual(menuTrigger("/rev", 4), { sigil: "/", query: "rev", start: 0, end: 4 });
  assert.deepEqual(menuTrigger("  /", 3), { sigil: "/", query: "", start: 2, end: 3 });
  assert.deepEqual(menuTrigger("fix /rev", 8), { sigil: "/", query: "rev", start: 4, end: 8 });
  assert.equal(menuTrigger("a/b", 3), null, "paths are not triggers");
  assert.deepEqual(menuTrigger("use $pd now", 7), { sigil: "$", query: "pd", start: 4, end: 7 });
  assert.deepEqual(menuTrigger("$pdfx", 2), { sigil: "$", query: "p", start: 0, end: 5 }, "the whole token is replaced");
  assert.equal(menuTrigger("costs $5 and", 12), null);
  assert.equal(menuTrigger("x $a b", 6), null, "the caret left the token");
  assert.deepEqual(
    menuTrigger("see @src/lib/x.ts", 17),
    { sigil: "@", query: "src/lib/x.ts", start: 4, end: 17 },
    "file queries keep their slashes",
  );
});

test("files: @ lists workspace paths", () => {
  const all = [file("src/lib/x.ts"), file("README.md"), cmd("review")];
  const trigger = { sigil: "@", query: "src", start: 0, end: 4 };
  assert.deepEqual(menuItems(all, trigger).map((item) => item.name), ["src/lib/x.ts"]);
  assert.equal(applyItem("see @src", menuTrigger("see @src", 8), file("src/lib/x.ts")).text, "see @src/lib/x.ts ");
});

test("items: ranking and which menu lists what", () => {
  const all = [
    cmd("review", { description: "Review the diff" }),
    cmd("compact"),
    cmd("pr-comments"),
    cmd("auto", { insert: "$auto " }),
    skill("pdf", "$pdf "),
    skill("pr-review", "/skill:pr-review "),
  ];
  const names = (sigil, query) => menuItems(all, { sigil, query, start: 0, end: 0 }).map((i) => i.name);
  assert.deepEqual(names("/", "re"), ["review", "pr-review"]);
  assert.deepEqual(names("/", "pr"), ["pr-comments", "pr-review"]);
  assert.deepEqual(names("/", "auto"), ["auto"], "Codex $ commands are listed under / too");
  assert.deepEqual(names("/", "diff"), ["review"], "descriptions match last");
  assert.deepEqual(names("$", ""), ["pdf"], "only dollar-style skills under $");
  assert.ok(names("/", "").includes("pr-review"), "slash-style skills are listed under /");
  assert.ok(names("/", "").includes("pdf"), "the unified picker includes Codex skills");
  const trigger = menuTrigger("/pd", 3);
  assert.equal(applyItem("/pd", trigger, menuItems(all, trigger)[0]).text, "$pdf ");
  const duplicate = [cmd("dup"), skill("dup", "/dup ")];
  assert.deepEqual(menuItems(duplicate, { sigil: "/", query: "", start: 0, end: 0 }).map((i) => i.kind), ["command"]);
  const dollar = [cmd("deploy", { insert: "$deploy " }), skill("deploy", "$deploy ")];
  assert.deepEqual(menuItems(dollar, { sigil: "$", query: "", start: 0, end: 0 }).map((i) => i.kind), ["skill"]);
});

test("OpenCode skills match display name or folder ID and insert a case-sensitive mention", () => {
  const selected = skill("PDF toolkit", "@Extract-Docs ");
  const entries = [file("Extract-Docs"), cmd("extract"), selected];
  const trigger = menuTrigger("Use /extract-do", 15);
  assert.deepEqual(menuItems(entries, trigger), [selected]);
  assert.deepEqual(applyItem("Use /extract-do", trigger, selected), { text: "Use @Extract-Docs ", caret: 18 });
  assert.deepEqual(menuItems(entries, menuTrigger("/pdf", 4)), [selected]);
  assert.deepEqual(menuItems(entries, menuTrigger("@extract", 8)), [entries[0]]);
});

test("command and skill abbreviations match after direct names", () => {
  const review = cmd("review");
  const toolkit = skill("Design toolkit", "$design-toolkit ");
  const exact = cmd("rvw");
  assert.deepEqual(menuItems([review, exact], menuTrigger("/rvw", 4)), [exact, review]);
  assert.deepEqual(menuItems([review, toolkit], menuTrigger("$dtk", 4)), [toolkit]);
  assert.deepEqual(menuItems([file("src/review.ts")], menuTrigger("@rvw", 4)), []);
});

test("Cabin commands are searchable alongside the agent's own command", () => {
  const native = cmd("context", { source: "agent" });
  const all = [native, ...sessionCommands];
  const trigger = menuTrigger("/context", 8);
  assert.deepEqual(menuItems(all, trigger).map((item) => item.insert.trim()), ["/context", "/cabin:context"]);
  assert.equal(menuItems(all, menuTrigger("/cabin:st", 9))[0].insert.trim(), "/cabin:status");
});

test("Cabin commands only handle a standalone command without attachments", () => {
  assert.equal(sessionCommandFor(" /cabin:status ", false), "status");
  assert.equal(sessionCommandFor("/cabin:context", false), "context");
  assert.equal(sessionCommandFor("/cabin:context", true), null);
  for (const text of ["/context", "/status", "/mcp", "/cabin:status extra", "Show /cabin:status"]) {
    assert.equal(sessionCommandFor(text, false), null, text);
  }
});

test("completion holds Enter even with no selection, but leaves newline and focus navigation alone", () => {
  assert.equal(menuKeyAction("Enter", false, false), "hold");
  assert.equal(menuKeyAction("Escape", false, false), "dismiss");
  assert.equal(menuKeyAction("Enter", false, true), "pick");
  assert.equal(menuKeyAction("Tab", false, true), "pick");
  assert.equal(menuKeyAction("Tab", false, false), null);
  assert.equal(menuKeyAction("Tab", true, true), null);
  assert.equal(menuKeyAction("Enter", true, true), null);
  assert.equal(menuKeyAction("ArrowUp", false, true), "previous");
  assert.equal(menuKeyAction("ArrowDown", false, true), "next");
  assert.equal(menuKeyAction("Enter", false, true, true), "hold");
  assert.equal(menuKeyAction("Tab", false, true, true), "hold");
  assert.equal(menuKeyAction("Escape", false, true, true), "hold");
  assert.equal(menuKeyAction("ArrowDown", false, true, true), "next");
});

test("apply: native syntax and no doubled spaces", () => {
  const t = menuTrigger("/rev", 4);
  assert.deepEqual(applyItem("/rev", t, cmd("review")), { text: "/review ", caret: 8 });
  const s = menuTrigger("use $p please", 6);
  assert.deepEqual(applyItem("use $p please", s, skill("pdf", "$pdf ")), { text: "use $pdf please", caret: 9 });
  const m = menuTrigger("then /pr", 8);
  assert.equal(applyItem("then /pr", m, skill("pr-review", "/skill:pr-review ")).text, "then /skill:pr-review ");
  const first = menuTrigger("$pr", 3);
  assert.equal(applyItem("$pr", first, skill("pr-review", "/skill:pr-review ")).text, "/skill:pr-review ");
});
