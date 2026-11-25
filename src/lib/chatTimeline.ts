/**
 * Provider-independent transcript projection.
 *
 * The backend already owns the hard part: ACP updates become durable
 * `ChatItem`s, `turnId` groups them by prompt, and tool items carry their
 * subject/files/changes. This module only decides what a conversation reads
 * like. It deliberately does no provider-specific interpretation:
 *
 * - a turn is one user prompt plus everything the runtime attributed to it;
 * - the whole assistant body of a turn is one activity group, so its process
 *   reads as a continuous timeline under one summary capsule;
 * - the last assistant message is the turn's answer, everything between the
 *   steps is progress;
 * - approvals and questions that are waiting do not enter the timeline (they
 *   are blocking docks above the composer);
 * - changed files are summarized once at the end of their turn.
 */
import type {
  ChatItem, ChatRunState, FileChangeView, ToolAction,
} from "../services/ipc/generated/core";

export type TimelineRow = { turn: string } & (
  | { kind: "user"; id: string; item: ChatItem }
  | {
      kind: "turn";
      id: string;
      /** Everything the runtime attributed to the turn, in order. */
      items: ChatItem[];
      /** The last assistant message: the reply's final answer. */
      answerId: string | null;
      elapsed: number | null;
      /** The newest turn while the agent runs: its steps stay open. */
      current: boolean;
      changes: FileChangeView[];
    }
);

export interface TimelineInput {
  items: ChatItem[];
  state: ChatRunState;
}

export interface ActiveTurn {
  turn: string;
  since: number | null;
  current: ChatItem | null;
  steps: ChatItem[];
  waiting: boolean;
  streaming: boolean;
}

/** `array.at(-1)`; the UI targets ES2020. */
export function last<T>(values: readonly T[]): T | undefined {
  return values[values.length - 1];
}

const TOOL_KINDS = new Set<ChatItem["kind"]>([
  "tool",
  "command_execution",
  "file_change",
]);
const ACTIVE_STATES = new Set<ChatRunState>(["running", "waiting", "starting"]);

export function isToolItem(item: ChatItem): boolean {
  return TOOL_KINDS.has(item.kind);
}

export function isWork(item: ChatItem): boolean {
  return isToolItem(item) || item.kind === "reasoning";
}

export function actionOf(item: ChatItem): ToolAction {
  if (item.action) return item.action;
  if (item.kind === "command_execution") return "command";
  if (item.kind === "file_change") return "edit";
  return "other";
}

function prettyTool(title: string): string {
  const name = last(title.split("__")) ?? title;
  return name ? name.replace(/[_-]+/g, " ") : "a tool";
}

/** One line for a single work item. The verb is derived from structured ACP
 * data, never from the agent's name. */
export function entryLabel(item: ChatItem): string {
  if (item.kind === "reasoning") {
    return item.status === "running" ? "Thinking" : "Thought";
  }
  if (item.kind === "proposed_plan") return item.title || "Proposed a plan";
  const subject = item.subject;
  const live = item.status === "running";
  switch (actionOf(item)) {
    case "read":
      return `${live ? "Reading" : "Read"} ${subject || "a file"}`;
    case "edit":
      return `${live ? "Editing" : "Edited"} ${subject || "files"}`;
    case "command":
      return subject
        ? `${live ? "Running" : "Ran"} ${subject}`
        : live ? "Running a command" : "Ran a command";
    case "code-search":
      return `${live ? "Searching" : "Searched"} ${subject || "the code"}`;
    case "web-search":
      return `${live ? "Searching the web for" : "Searched the web for"} ${subject || "…"}`;
    case "plan":
      return live ? "Updating the plan" : "Updated the plan";
    case "agent":
      return `${live ? "Running subagent" : "Subagent"}${subject ? `: ${subject}` : ""}`;
    case "other":
      return `${live ? "Using" : "Used"} ${prettyTool(item.title)}`;
  }
}

/** A short gist for a reasoning block: the bold heading, else the first
 * sentence, without markdown. */
export function thoughtLine(markdown: string): string {
  const heading = /^\s*\*\*([^*\r\n]+)\*\*[ \t]*\r?(?:\n|$)/.exec(markdown)?.[1];
  const text = (heading ?? markdown)
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/^[ \t]*(?:#{1,6}|[-*+]|\d+\.)[ \t]+/gm, "")
    .replace(/`+|\*\*|~~/g, "")
    .replace(/\s+/g, " ")
    .trim();
  if (heading !== undefined) return text;
  const end = /[.?!]["'”’)]?(?=\s)/.exec(text);
  return end ? text.slice(0, end.index + end[0].length) : text;
}

/** Whole seconds: 12s, 1m 05s, 1h 02m. */
export function elapsedLabel(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${String(seconds % 60).padStart(2, "0")}s`;
  return `${Math.floor(minutes / 60)}h ${String(minutes % 60).padStart(2, "0")}m`;
}

interface Turn {
  id: string;
  user: ChatItem | null;
  body: ChatItem[];
}

function splitTurns(items: ChatItem[]): Turn[] {
  const turns: Turn[] = [];
  let current: Turn = { id: "start", user: null, body: [] };
  for (const item of items) {
    if (item.kind === "user_message") {
      const turnId = item.turnId || item.id;
      // A steer lands inside the turn it interrupts; it is part of that
      // conversation, not a new prompt. Only the prompt that opens a turn
      // starts one.
      if (current.user && turnId === current.id) {
        current.body.push(item);
        continue;
      }
      if (current.user || current.body.length) turns.push(current);
      current = { id: turnId, user: item, body: [] };
      continue;
    }
    const turnId = item.turnId || current.id;
    if (turnId !== current.id) {
      if (current.user || current.body.length) {
        turns.push(current);
        current = { id: turnId, user: null, body: [] };
      } else {
        // A turn opened by the agent (or a child session) has its own id.
        current.id = turnId;
      }
    }
    current.body.push(item);
  }
  if (current.user || current.body.length) turns.push(current);
  return turns;
}

function turnElapsed(turn: Turn): number | null {
  const starts = [turn.user, ...turn.body].flatMap((item) =>
    item && item.startedAt > 0 ? [item.startedAt] : [],
  );
  const ends = turn.body.flatMap((item) =>
    item.updatedAt > 0 ? [item.updatedAt] : [],
  );
  if (!starts.length || !ends.length) return null;
  return Math.max(0, Math.max(...ends) - Math.min(...starts));
}

/**
 * The turn's changed files with their structured changes. A path the agent
 * reported more than once keeps its latest change; items without structured
 * changes contribute a path-only entry.
 */
function changedFiles(body: ChatItem[]): FileChangeView[] {
  const byPath = new Map<string, FileChangeView>();
  for (const item of body) {
    if (!isWork(item) || actionOf(item) !== "edit" || item.status === "failed") continue;
    for (const change of item.changes ?? []) byPath.set(change.path, change);
    for (const file of item.files) {
      if (!byPath.has(file)) {
        byPath.set(file, {
          path: file,
          origin: "provider",
          kind: "unknown",
          additions: null,
          deletions: null,
          patch: null,
        });
      }
    }
  }
  return [...byPath.values()];
}

/** Latest reported change per path; these are event details, not a Git net diff. */
export function sessionChanges(rows: TimelineRow[]): FileChangeView[] {
  const byPath = new Map<string, FileChangeView>();
  for (const row of rows) {
    if (row.kind === "turn") for (const change of row.changes) byPath.set(change.path, change);
  }
  return [...byPath.values()];
}

export function deriveTimeline({ items, state }: TimelineInput): TimelineRow[] {
  const rows: TimelineRow[] = [];
  const turns = splitTurns(items);
  const live = ACTIVE_STATES.has(state);

  turns.forEach((turn) => {
    if (turn.user) {
      rows.push({ turn: turn.id, kind: "user", id: turn.user.id, item: turn.user });
    }
    const latest = live && turn === last(turns);
    const body = turn.body.filter((item) =>
      !(
        (item.kind === "approval_request" || item.kind === "user_input_request")
        && item.status === "waiting"
      )
    );
    const answer = [...body]
      .reverse()
      .find((item) => item.kind === "assistant_message") ?? null;
    // A prompt with no reply and a settled chat has nothing to head; a live
    // one keeps its capsule so the pending reply has where to appear.
    if (body.length > 0 || latest) {
      rows.push({
        turn: turn.id,
        kind: "turn",
        id: `turn:${turn.id}`,
        items: body,
        answerId: answer?.id ?? null,
        elapsed: turnElapsed(turn),
        current: latest,
        changes: changedFiles(turn.body),
      });
    }
  });

  return rows;
}

/** The current turn's live activity, rendered immediately above the composer.
 * It answers "what is happening now" without hiding what already happened in
 * the transcript. */
export function activeTurn(
  items: ChatItem[],
  state: ChatRunState,
): ActiveTurn | null {
  if (!ACTIVE_STATES.has(state)) return null;
  const turns = splitTurns(items);
  const turn = last(turns);
  if (!turn || (!turn.user && !turn.body.length)) return null;
  const steps = turn.body.filter(isWork);
  // Only a step that is actually in flight is "current". A finished thought
  // must not keep the live line saying "Working <old thought>" while the
  // agent is already writing its answer.
  const current = [...steps].reverse().find((item) =>
    item.status === "running" || item.status === "pending"
  ) ?? null;
  const since = turn.user?.startedAt
    || turn.body.find((item) => item.startedAt > 0)?.startedAt
    || null;
  return {
    turn: turn.id,
    since,
    current,
    steps,
    waiting: state === "waiting",
    streaming: turn.body.some(
      (item) => item.kind === "assistant_message" && item.status === "running",
    ),
  };
}

/** The oldest waiting approval or question; the composer shows only this one
 * so a later request can never bypass the native waiting point. */
export function pendingRequest(items: ChatItem[]): ChatItem | null {
  return items.find(
    (item) =>
      (item.kind === "approval_request" || item.kind === "user_input_request")
      && item.status === "waiting"
      && item.requestId != null,
  ) ?? null;
}

export type StepGroup = { kind: "commands" | "explore" | "single"; items: ChatItem[] };

/** Fewer reads and searches than this are clearer as their own rows. */
const EXPLORE_MIN = 3;

function groupKind(item: ChatItem): "commands" | "explore" | null {
  // Delegated work and failures keep their own row.
  if (!isToolItem(item) || item.taskId) return null;
  const action = actionOf(item);
  if (action === "command") return "commands";
  return (action === "read" || action === "code-search") && item.status !== "failed" ? "explore" : null;
}

/** Runs of shell commands, and of reads and searches, each read as one step. */
export function groupSteps(items: ChatItem[]): StepGroup[] {
  const groups: StepGroup[] = [];
  for (let index = 0; index < items.length;) {
    const kind = groupKind(items[index]);
    let end = index + 1;
    while (kind && end < items.length && groupKind(items[end]) === kind) end++;
    const run = items.slice(index, end);
    if (kind && run.length >= (kind === "commands" ? 2 : EXPLORE_MIN)) groups.push({ kind, items: run });
    else for (const item of run) groups.push({ kind: "single", items: [item] });
    index = end;
  }
  return groups;
}
