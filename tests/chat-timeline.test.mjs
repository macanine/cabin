import assert from "node:assert/strict";
import test from "node:test";
import {
  activeTurn,
  deriveTimeline,
  groupSteps,
  elapsedLabel,
  entryLabel,
  pendingRequest,
  sessionChanges,
  thoughtLine,
} from "../src/lib/chatTimeline.ts";

let ordinal = 0;

function item(kind, fields = {}) {
  ordinal += 1;
  return {
    id: fields.id ?? `i${ordinal}`,
    ordinal,
    revision: 1,
    turnId: fields.turnId ?? "u1",
    kind,
    status: "completed",
    title: "",
    text: "",
    detail: "",
    requestId: null,
    questions: [],
    attachments: [],
    startedAt: 1000 * ordinal,
    updatedAt: 1000 * ordinal + 500,
    action: null,
    subject: "",
    files: [],
    changes: [],
    taskId: null,
    ...fields,
  };
}

const tool = (action, subject, fields = {}) =>
  item(action === "command" ? "command_execution" : "tool", {
    action,
    subject,
    ...fields,
  });

const kinds = (rows) => rows.map((row) => row.kind);

test("session changes keep the latest reported change per path across turns", () => {
  const change = (path, additions, deletions) => ({ path, additions, deletions, origin: "provider", kind: "modified", patch: null });
  const items = [
    item("user_message", { id: "u1", turnId: "u1" }),
    tool("edit", "a", { turnId: "u1", files: ["a"], changes: [change("a", 10, 2)] }),
    item("user_message", { id: "u2", turnId: "u2" }),
    tool("edit", "a", { turnId: "u2", files: ["a", "binary"], changes: [change("a", 2, 1), change("binary", null, null)] }),
  ];
  const changes = sessionChanges(deriveTimeline({ items, state: "ready" }));
  assert.deepEqual(changes.map(({ path, additions, deletions }) => ({ path, additions, deletions })), [
    { path: "a", additions: 2, deletions: 1 }, { path: "binary", additions: null, deletions: null },
  ]);
});

test("groups a flat transcript by stable turn ids, not by position", () => {
  ordinal = 0;
  const items = [
    item("user_message", { id: "u1", turnId: "u1", text: "first" }),
    item("assistant_message", { id: "a1", turnId: "u1", text: "one" }),
    item("user_message", { id: "u2", turnId: "u2", text: "second" }),
    item("assistant_message", { id: "a2", turnId: "u2", text: "two" }),
  ];

  const rows = deriveTimeline({ items, state: "ready" });

  assert.deepEqual(kinds(rows), ["user", "turn", "user", "turn"]);
  assert.deepEqual(
    rows.map((row) => row.turn),
    ["u1", "u1", "u2", "u2"],
  );
  assert.equal(rows[0].item.id, "u1");
  assert.equal(rows[3].answerId, "a2");
});

test("keeps every ACP tool call visible while grouping consecutive work", () => {
  ordinal = 0;
  const items = [
    item("user_message", { id: "u1", text: "fix it" }),
    item("reasoning", { id: "r1", text: "**Checking the build**\n\nI will inspect it." }),
    tool("read", "src/a.ts", { id: "t1" }),
    tool("command", "npm test", { id: "t2" }),
    item("assistant_message", { id: "a1", text: "Looking…" }),
    tool("edit", "src/a.ts", { id: "t3", files: ["src/a.ts"] }),
    item("assistant_message", { id: "a2", text: "Done" }),
  ];

  const rows = deriveTimeline({ items, state: "ready" });

  assert.deepEqual(kinds(rows), ["user", "turn"]);
  assert.deepEqual(
    rows[1].items.map((entry) => entry.id),
    ["r1", "t1", "t2", "a1", "t3", "a2"],
  );
  assert.equal(rows[1].answerId, "a2");
  assert.deepEqual(rows[1].changes.map((change) => change.path), ["src/a.ts"]);
});

test("the live turn keeps every activity group open without hiding prior steps", () => {
  ordinal = 0;
  const items = [
    item("user_message", {
      id: "u1",
      startedAt: 5_000,
      updatedAt: 5_100,
    }),
    tool("read", "a", { id: "t1", status: "completed" }),
    item("assistant_message", { id: "a1", text: "Checking the build" }),
    tool("read", "b", { id: "t2", status: "completed" }),
    tool("command", "cargo build", {
      id: "t3",
      status: "running",
      startedAt: 9_000,
      updatedAt: null,
    }),
  ];

  const live = activeTurn(items, "running");

  assert.equal(live.turn, "u1");
  assert.equal(live.since, 5_000);
  assert.equal(live.current.id, "t3");
  assert.deepEqual(live.steps.map((entry) => entry.id), ["t1", "t2", "t3"]);
  assert.equal(live.waiting, false);
  assert.equal(live.streaming, false);

  const rows = deriveTimeline({ items, state: "running" });
  assert.equal(rows[1].kind, "turn");
  assert.equal(rows[1].current, true, "the live turn's process stays open");

  const settled = deriveTimeline({ items, state: "ready" });
  assert.equal(settled[1].current, false);
});

test("a waiting turn reports its blocking state and a streaming assistant", () => {
  ordinal = 0;
  const items = [
    item("user_message", { id: "u1", startedAt: 2_000 }),
    item("assistant_message", {
      id: "a1",
      status: "running",
      startedAt: 3_000,
      updatedAt: 4_000,
    }),
  ];

  const live = activeTurn(items, "waiting");

  assert.equal(live.current, null);
  assert.equal(live.streaming, true);
  assert.equal(live.waiting, true);
  assert.equal(activeTurn(items, "ready"), null);
});

test("only the earliest blocking approval or question enters the dock", () => {
  ordinal = 0;
  const first = item("approval_request", {
    id: "approval-1",
    status: "waiting",
    requestId: "r1",
    approval: {
      kind: "file-change",
      decisions: [
        { decision: "decline", label: "Decline", warning: null },
        { decision: "accept", label: "Approve", warning: null },
      ],
    },
  });
  const second = item("user_input_request", {
    id: "question-1",
    status: "waiting",
    requestId: "r2",
    questions: [{ question: "Which branch?", options: [] }],
  });

  assert.equal(pendingRequest([item("user_message"), first, second]), first);
  assert.equal(pendingRequest([{ ...first, status: "completed" }, second]), second);
  assert.equal(pendingRequest([{ ...first, requestId: null }]), null);
});

test("pending requests do not become transcript cards", () => {
  ordinal = 0;
  const approval = item("approval_request", {
    status: "waiting",
    requestId: "r1",
    title: "Allow Bash?",
  });
  const items = [
    item("user_message", { id: "u1" }),
    tool("command", "sleep 100", { status: "interrupted" }),
    item("error", { id: "e1", text: "boom" }),
    approval,
  ];

  const rows = deriveTimeline({ items, state: "ready" });

  assert.deepEqual(kinds(rows), ["user", "turn"]);
  assert.equal(rows[1].items[0].status, "interrupted");
  assert.ok(rows[1].items.some((entry) => entry.kind === "error"));
  assert.ok(!rows[1].items.some((entry) => entry.id === approval.id));
});

test("changes merge by path and retain path-only reports", () => {
  ordinal = 0;
  const change = (path, patch, fields = {}) => ({
    path,
    origin: "provider",
    kind: "added",
    additions: 1,
    deletions: 0,
    patch,
    ...fields,
  });
  const items = [
    item("user_message", { id: "u1", text: "write notes" }),
    tool("edit", "notes.txt", {
      files: ["notes.txt"],
      changes: [
        change(
          "notes.txt",
          "--- /dev/null\n+++ b/notes.txt\n@@ -0,0 +1,1 @@\n+one\n",
        ),
      ],
    }),
    tool("edit", "notes.txt", {
      files: ["notes.txt"],
      changes: [
        change(
          "notes.txt",
          "--- a/notes.txt\n+++ b/notes.txt\n@@ -1,1 +1,1 @@\n-one\n+two\n",
          { kind: "modified" },
        ),
      ],
    }),
    tool("edit", "old.txt", { files: ["old.txt"] }),
    tool("edit", "broken.txt", { files: ["broken.txt"], status: "failed" }),
    item("assistant_message", { id: "final", text: "ok" }),
  ];

  const rows = deriveTimeline({ items, state: "ready" });
  const changes = rows.find((row) => row.kind === "turn").changes;

  assert.deepEqual(changes.map((change) => change.path), ["notes.txt", "old.txt"]);
  const notes = changes.find((entry) => entry.path === "notes.txt");
  assert.equal(notes.kind, "modified", "the later report of a path wins");
  assert.match(notes.patch, /\+two/);
  const legacy = changes.find((entry) => entry.path === "old.txt");
  assert.deepEqual([legacy.kind, legacy.patch], ["unknown", null]);
  assert.equal(
    changes.some((entry) => entry.path === "broken.txt"),
    false,
  );
});

test("a settled step is not reported as the live one", () => {
  ordinal = 0;
  const items = [
    item("user_message", { id: "u1" }),
    item("reasoning", { id: "r1", status: "completed", text: "All 8 subagents completed." }),
    item("assistant_message", { id: "a1", status: "running", text: "Summarising" }),
  ];

  const live = activeTurn(items, "running");

  assert.equal(live.current, null);
  assert.equal(live.streaming, true);
});

test("a steer stays inside the turn it interrupts", () => {
  ordinal = 0;
  const items = [
    item("user_message", { id: "u1", turnId: "u1", text: "start" }),
    tool("read", "a", { id: "t1" }),
    item("user_message", { id: "s1", turnId: "u1", text: "also check b" }),
    tool("read", "b", { id: "t2" }),
    item("assistant_message", { id: "a1", turnId: "u1", text: "done" }),
  ];

  const rows = deriveTimeline({ items, state: "ready" });

  assert.deepEqual(kinds(rows), ["user", "turn"]);
  assert.equal(rows[0].item.id, "u1");
  assert.deepEqual(
    rows[1].items.map((entry) => entry.id),
    ["t1", "s1", "t2", "a1"],
  );
  assert.equal(rows[1].answerId, "a1");
});

test("work labels are derived only from structured action facts", () => {
  assert.equal(entryLabel(tool("command", "npm test")), "Ran npm test");
  assert.equal(
    entryLabel(tool("read", "src/a.ts", { status: "running" })),
    "Reading src/a.ts",
  );
  assert.equal(
    entryLabel(tool("other", "", { title: "mcp__github__create_issue" })),
    "Used create issue",
  );
  assert.equal(entryLabel(tool("agent", "reviewer")), "Subagent: reviewer");
});

test("reasoning gist and elapsed labels stay compact", () => {
  assert.equal(
    thoughtLine("**Checking the schema**\n\nI will read store.rs."),
    "Checking the schema",
  );
  assert.equal(
    thoughtLine("Read `store.rs` first. Then the model."),
    "Read store.rs first.",
  );
  assert.equal(elapsedLabel(5_400), "5s");
  assert.equal(elapsedLabel(65_000), "1m 05s");
  assert.equal(elapsedLabel(3_725_000), "1h 02m");
});

test("runs of commands and of reads group; failures and subagents keep their rows", () => {
  const step = (id, action, over = {}) => ({ id, kind: "tool", action, status: "completed", taskId: null, ...over });
  const shape = (items) => groupSteps(items).map((g) => `${g.kind}:${g.items.length}`);
  assert.deepEqual(shape([step("1", "read"), step("2", "code-search"), step("3", "read"), step("4", "command"), step("5", "command")]), ["explore:3", "commands:2"]);
  assert.deepEqual(shape([step("1", "read"), step("2", "read")]), ["single:1", "single:1"]);
  assert.deepEqual(shape([step("1", "read"), step("2", "read"), step("3", "read", { status: "failed" }), step("4", "command", { taskId: "t" })]),
    ["single:1", "single:1", "single:1", "single:1"]);
});
