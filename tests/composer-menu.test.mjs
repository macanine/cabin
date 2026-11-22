import { test } from "node:test";
import assert from "node:assert/strict";
import { applyItem, menuItems, menuTrigger } from "../src/lib/composerMenu.ts";

const cmd = (name, extra = {}) => ({ name, description: null, hint: null, kind: "command", source: null, insert: `/${name} `, ...extra });
const skill = (name, insert, extra = {}) => ({ ...cmd(name), kind: "skill", insert, ...extra });
const file = (path) => ({ ...cmd(path), kind: "file", source: "workspace", insert: `@${path} ` });

test("triggers: / and $ after whitespace, @ anywhere", () => {
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
  assert.ok(!names("/", "").includes("pdf"), "a $-style skill is not a slash command");
  const duplicate = [cmd("dup"), skill("dup", "/dup ")];
  assert.deepEqual(menuItems(duplicate, { sigil: "/", query: "", start: 0, end: 0 }).map((i) => i.kind), ["command"]);
  const dollar = [cmd("deploy", { insert: "$deploy " }), skill("deploy", "$deploy ")];
  assert.deepEqual(menuItems(dollar, { sigil: "$", query: "", start: 0, end: 0 }).map((i) => i.kind), ["skill"]);
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
