import {
  IconAlertCircle, IconBrain, IconCheck, IconEye, IconHammer,
  IconInfoCircle, IconListCheck, IconPencil, IconRobot, IconSearch,
  IconTerminal2, IconTool, IconWorld,
} from "@tabler/icons-react";
import type { ReactNode } from "react";
import type { ChatAsyncTask, ChatItem, SubagentTask, ToolAction } from "../../services/ipc/generated/core";
import { subagentElapsed, taskStateLabel, taskTone } from "../../lib/chatTasks";
import {
  actionOf, elapsedLabel, entryLabel, groupSteps, isToolItem, thoughtLine,
} from "../../lib/chatTimeline";
import { formatDuration } from "../../lib/format";
import { useChat } from "./context";
import { FileChanges } from "./FileChanges";
import { Markdown } from "./Markdown";
import { ActivityRow, CopyAction, DetailBox, PRE, RailMark } from "./ActivityRow";
import { SweepText } from "./LiveLine";
import { Stat } from "../diff/Stat";

const ICONS: Record<ToolAction, typeof IconEye> = {
  read: IconEye,
  edit: IconPencil,
  command: IconTerminal2,
  "code-search": IconSearch,
  "web-search": IconWorld,
  plan: IconListCheck,
  agent: IconRobot,
  other: IconTool,
};

function firstLine(text: string): string {
  return text.split("\n")[0] ?? text;
}

function sumLines(text: string | null | undefined): number {
  return text ? text.split("\n").length : 0;
}

function tailLines(text: string, count: number): string {
  return text.replace(/\n+$/, "").split("\n").filter((line) => line.trim()).slice(-count).join("\n");
}

/** The newest line of a thought, kept short enough to read in one row. */
function latestThought(text: string): string {
  const lines = text.trimEnd().split("\n").map((line) => line.trim()).filter(Boolean);
  const latest = lines[lines.length - 1] ?? "";
  const limit = /[\u3400-\u9fff]/.test(latest) ? 40 : 90;
  return latest.length > limit ? `…${latest.slice(-limit)}` : latest;
}

function Duration({ item, live }: { item: ChatItem; live: boolean }) {
  const { now } = useChat();
  if (!item.startedAt) return null;
  if (live) {
    return <span className="text-accent">{elapsedLabel(Math.max(0, now - item.startedAt))}</span>;
  }
  const ms = item.updatedAt - item.startedAt;
  if (item.kind === "reasoning" || ms < 500) return null;
  return <span className="text-faint">{formatDuration(ms)}</span>;
}

/** A tool card that started background work shows that work's state, not
 * the (long finished) launch call's. */
function TaskBadge({ task }: { task: ChatAsyncTask }) {
  const tone = taskTone(task.state);
  return (
    <span className={`inline-flex items-center gap-1.5 rounded-full px-2 py-px text-xs ${tone === "live" ? "bg-accent-soft text-accent" : tone === "bad" ? "bg-bad/12 text-bad" : "bg-hover text-muted"}`} title={task.description || task.name}>
      {tone === "live" && <span className="size-1.5 rounded-full bg-accent" aria-hidden />}
      {tone === "live" ? "In background" : taskStateLabel(task.state)}
    </span>
  );
}

/** A subagent launch shows the subagent: live time, or how it ended. */
function SubagentBadge({ task }: { task: SubagentTask }) {
  const { now } = useChat();
  const elapsed = elapsedLabel(subagentElapsed(task, now));
  if (task.state === "active") return <span className="inline-flex items-center gap-1.5 rounded-full bg-accent-soft px-2 py-px text-xs text-accent"><span className="size-1.5 rounded-full bg-accent" aria-hidden />{elapsed}</span>;
  if (task.state === "failed") return <span className="text-xs font-medium text-bad">failed · {elapsed}</span>;
  if (task.state === "stopped") return <span className="text-faint">stopped · {elapsed}</span>;
  return <span className="text-faint">{elapsed}</span>;
}

/** The row's leading text. The ring icon says what kind of step it is; a
 * missing subject falls back to the verb. */
/** The verb the row says: every tool row has words, not just a file path. */
function toolVerb(item: ChatItem): string {
  const live = item.status === "running" || item.status === "pending";
  switch (actionOf(item)) {
    case "read":
      return live ? "Reading" : "Read";
    case "edit":
      return live ? "Editing" : "Edited";
    case "command":
      return live ? "Running" : "Ran";
    case "code-search":
      return live ? "Searching" : "Searched";
    case "web-search":
      return live ? "Searching the web" : "Searched the web";
    case "plan":
      return live ? "Updating plan" : "Updated plan";
    case "agent":
      return "Subagent";
    case "other":
      return live ? "Using" : "Used";
  }
}

/** The object of the verb: a path, a command, a query. */
function toolSubject(item: ChatItem): string {
  const subject = item.subject;
  switch (actionOf(item)) {
    case "read":
      return subject || "a file";
    case "edit":
      return subject || item.files[0] || "files";
    case "command":
      return subject ? firstLine(subject) : "a command";
    case "code-search":
      return subject || "the code";
    case "web-search":
      return subject;
    case "plan":
      return subject;
    case "agent":
      return subject;
    case "other": {
      const title = item.title || "tool";
      return subject || (title.split("__").pop() ?? title).replace(/[_-]+/g, " ");
    }
  }
}

function toolFigure(item: ChatItem): ReactNode {
  const live = item.status === "running" || item.status === "pending";
  if (item.status === "failed") return <span className="text-xs font-medium text-bad">failed</span>;
  if (live) return <Duration item={item} live />;
  if (actionOf(item) === "edit" && item.changes.length > 0) {
    const add = item.changes.reduce((total, change) => total + (change.additions ?? 0), 0);
    const del = item.changes.reduce((total, change) => total + (change.deletions ?? 0), 0);
    if (add || del) {
      return <Stat additions={add} deletions={del} />;
    }
  }
  return <Duration item={item} live={false} />;
}

function ToolDetail({ item }: { item: ChatItem }) {
  const isCommand = actionOf(item) === "command" || item.kind === "command_execution";
  const output = item.detail;
  if (item.changes.length > 0) {
    return (
      <FileChanges
        changes={item.changes}
        title={item.files.length > 1 ? `${item.files.length} files` : undefined}
        open
      />
    );
  }
  // The row already carries the command; only a command it had to cut short
  // is repeated here, in full. Non-command tools show their input instead.
  const command = isCommand ? item.text.trim() : "";
  const showCommand = isCommand && item.subject.endsWith("…") && Boolean(command);
  const input = isCommand ? "" : item.text;
  const showInput = Boolean(input && input !== "{}");
  if (!showCommand && !showInput && !output) return <p className="m-0 px-1 py-1 text-sm text-faint">No details reported</p>;
  const outputLines = sumLines(output);
  return (
    <DetailBox
      footer={output ? `${outputLines} ${outputLines === 1 ? "line" : "lines"}` : null}
      actions={
        <>
          {isCommand && command && <CopyAction text={command} label="Copy command" />}
          {output && <CopyAction text={output} label="Copy output" />}
        </>
      }
      tone={item.status === "failed" ? "danger" : "neutral"}
    >
      {showCommand && (
        <pre className={PRE}>
          <span className="text-accent select-none">$ </span>
          <span className="text-fg">{command}</span>
        </pre>
      )}
      {showInput && <pre className={PRE}>{input}</pre>}
      {output && <pre className={`${PRE} ${showCommand || showInput ? "border-t border-line/60 pt-2" : ""}`}>{output}</pre>}
    </DetailBox>
  );
}

function ToolRow({ item }: { item: ChatItem }) {
  const { openSubagent, tasks, subagents } = useChat();
  const background = tasks.get(item.id);
  const subagentTask = item.taskId ? subagents.get(item.taskId) : undefined;
  const action = actionOf(item);
  const Icon = ICONS[action];
  const running = item.status === "running" || item.status === "pending";
  const isCommand = action === "command";
  const commandCut = isCommand && item.subject.endsWith("…");
  const hasInput = isCommand ? commandCut : Boolean(item.text && item.text !== "{}");
  const subagent = Boolean(item.taskId);
  const expandable = !subagent && (hasInput || Boolean(item.detail) || item.changes.length > 0);
  const liveTail = isCommand && running && item.detail ? tailLines(item.detail, 3) : "";
  const summary = background && !running && background.summary ? background.summary : "";
  // The launch call ends at once; the card follows the work it started.
  const shownStatus = background && item.status === "completed"
    ? (taskTone(background.state) === "live" ? "running" : background.state === "failed" ? "failed" : item.status)
    : item.status;
  return (
    <ActivityRow
      status={shownStatus}
      marker={<RailMark status={shownStatus}><Icon size={12} /></RailMark>}
      title={entryLabel(item)}
      label={
        <>
          <span className="shrink-0 font-medium">{toolVerb(item)}</span>
          <span
            className={`min-w-0 truncate text-muted ${isCommand ? "font-mono text-xs" : ""}`}
            dir="auto"
          >
            {toolSubject(item)}
          </span>
        </>
      }
      trailing={background && item.status !== "failed" ? <TaskBadge task={background} /> : subagentTask ? <SubagentBadge task={subagentTask} /> : toolFigure(item)}
      expandable={expandable}
      itemId={item.id}
      onActivate={item.taskId ? () => openSubagent(item.taskId!) : undefined}
      addon={liveTail ? <pre className={`${PRE} mt-1 max-h-[4.5rem] overflow-hidden rounded-lg bg-hover px-2.5 py-1.5`}>{liveTail}</pre> : summary ? <p className="m-0 mt-0.5 text-xs text-muted">{summary}</p> : undefined}
    >
      {!subagent && <ToolDetail item={item} />}
    </ActivityRow>
  );
}

function ReasoningRow({ item }: { item: ChatItem }) {
  const running = item.status === "running";
  // While the thought is being written, auto-open a compact rolling thought
  // window (dsh-claude-style reasoning-stream) and fold into a clean gist once done.
  const gist = running
    ? latestThought(item.text)
    : thoughtLine(item.text) || "Thought";
  return (
    <ActivityRow
      status={item.status}
      marker={<RailMark status={item.status}><IconBrain size={12} /></RailMark>}
      title={item.text}
      label={
        <>
          <SweepText text={running ? "Thinking" : "Thought"} active={running} className="shrink-0 font-medium" />
          {!running && gist && <span className="min-w-0 truncate text-muted">{gist}</span>}
        </>
      }
      trailing={<Duration item={item} live={running} />}
      expandable={Boolean(item.text)}
      defaultOpen={running}
    >
      <div className={`markdown text-[12.5px] text-muted ${running ? "chat-reason-window" : ""}`}>
        <Markdown text={item.text || "…"} streaming={running} compact />
      </div>
    </ActivityRow>
  );
}

/** Prose the agent wrote between steps: a dot on the rail, the words under it
 * as they were written. While a reply streams, its closing prose is also on
 * the rail; it is lifted below the steps once the turn settles. */
function ProseRow({ item }: { item: ChatItem }) {
  const running = item.status === "running";
  return (
    <li className="relative min-w-0 pl-8 before:absolute before:top-4 before:bottom-[-4px] before:left-[9.5px] before:w-px before:bg-line last:before:hidden" data-status={item.status}>
      <span className="absolute top-1 left-0 grid size-5 place-items-center"><RailMark status={item.status} dot /></span>
      <div className="markdown py-0.5 text-[12.5px] text-muted" dir="auto">
        <Markdown text={item.text} streaming={running} compact />
      </div>
    </li>
  );
}

function PlanRow({ item }: { item: ChatItem }) {
  return (
    <ActivityRow
      status={item.status}
      marker={<RailMark status={item.status}><IconListCheck size={12} /></RailMark>}
      label={<span className="font-medium">{item.title || "Proposed a plan"}</span>}
      expandable={Boolean(item.text)}
    >
      <div className="markdown text-muted"><Markdown text={item.text} /></div>
    </ActivityRow>
  );
}

function ErrorRow({ item }: { item: ChatItem }) {
  return (
    <ActivityRow
      status="failed"
      marker={<RailMark status="failed"><IconAlertCircle size={12} /></RailMark>}
      label={<span className="min-w-0 truncate text-bad">{item.text || item.title || "The agent reported an error"}</span>}
      trailing={item.detail ? <span className="text-xs font-medium text-bad">error</span> : null}
      expandable={Boolean(item.detail)}
    >
      <pre className={PRE}>{item.detail}</pre>
    </ActivityRow>
  );
}

function DecisionRow({ item }: { item: ChatItem }) {
  return (
    <ActivityRow
      status={item.status}
      marker={<RailMark status={item.status}><IconCheck size={12} /></RailMark>}
      label={<span className="min-w-0 truncate text-muted">{item.title || "Approval request"}</span>}
      trailing={<span className="text-faint">{item.status === "completed" ? "Done" : item.status}</span>}
      expandable={Boolean(item.text)}
    >
      <pre className={PRE}>{item.text || item.detail}</pre>
    </ActivityRow>
  );
}

function NoticeRow({ item }: { item: ChatItem }) {
  const compaction = item.title === "Context" || /compact/i.test(item.title);
  return (
    <ActivityRow
      status={item.status}
      marker={
        <RailMark status={item.status}>
          {compaction ? <IconHammer size={12} /> : <IconInfoCircle size={12} />}
        </RailMark>
      }
      label={
        <span className="min-w-0 truncate text-muted">
          {item.title && item.title !== "Context" && <strong className="mr-1.5 font-medium text-fg">{item.title}</strong>}
          {item.text || (item.title === "Context" ? "Conversation context updated" : item.title)}
        </span>
      }
      expandable={Boolean(item.detail)}
    >
      <pre className={PRE}>{item.detail}</pre>
    </ActivityRow>
  );
}

/** A message the user steered into a running turn: part of the turn, shown
 * as an interjection on its rail. */
function SteerRow({ item }: { item: ChatItem }) {
  return (
    <li className="relative min-w-0 pl-8 before:absolute before:top-4 before:bottom-[-4px] before:left-[9.5px] before:w-px before:bg-line last:before:hidden" data-status={item.status}>
      <span className="absolute top-1 left-0 grid size-5 place-items-center"><RailMark status={item.status} dot /></span>
      <div className="w-fit max-w-full rounded-xl border border-line bg-accent-soft/60 px-3 py-1.5 text-[13px] [overflow-wrap:anywhere]" dir="auto">{item.text}</div>
    </li>
  );
}

function StepRow({ item }: { item: ChatItem }) {
  if (item.kind === "reasoning") return <ReasoningRow item={item} />;
  if (item.kind === "assistant_message") return <ProseRow item={item} />;
  if (item.kind === "user_message") return <SteerRow item={item} />;
  if (item.kind === "proposed_plan") return <PlanRow item={item} />;
  if (item.kind === "error") return <ErrorRow item={item} />;
  if (item.kind === "approval_request" || item.kind === "user_input_request") {
    return <DecisionRow item={item} />;
  }
  if (isToolItem(item)) return <ToolRow item={item} />;
  return <NoticeRow item={item} />;
}

/** Consecutive shell commands read as one step: "Ran 5 commands · all passed". */
function CommandGroup({ items, current }: { items: ChatItem[]; current: boolean }) {
  const running = items.some((item) => item.status === "running" || item.status === "pending");
  const failed = items.filter((item) => item.status === "failed").length;
  const status = running ? "running" : failed ? "failed" : "completed";
  return (
    <ActivityRow
      status={status}
      marker={<RailMark status={status}><IconTerminal2 size={12} /></RailMark>}
      label={
        <span className="font-medium">{running ? "Running" : "Ran"} {items.length} commands</span>
      }
      trailing={
        failed > 0
          ? <span className="text-xs font-medium text-bad">{failed} failed</span>
          : !running
            ? <span className="text-faint">all passed</span>
            : null
      }
      expandable
      defaultOpen={current && running}
    >
      <ul className="m-0 grid list-none gap-0.5 p-0">
        {items.map((item) => <ToolRow key={item.id} item={item} />)}
      </ul>
    </ActivityRow>
  );
}

const plural = (count: number, word: string, many: string) => (count ? `${count} ${count === 1 ? word : many}` : null);

/** Looking around reads as one step too; the newest target shows while it runs. */
function ExploreGroup({ items }: { items: ChatItem[] }) {
  const active = items.filter((item) => item.status === "running" || item.status === "pending").pop();
  const status = active ? "running" : "completed";
  const reads = items.filter((item) => actionOf(item) === "read").length;
  const summary = [plural(reads, "file", "files"), plural(items.length - reads, "search", "searches")].filter(Boolean).join(", ");
  return (
    <ActivityRow
      status={status}
      marker={<RailMark status={status}><IconSearch size={12} /></RailMark>}
      label={<>
        <span className="shrink-0 font-medium">{active ? "Exploring" : "Explored"} {summary}</span>
        {active && <span className="min-w-0 truncate text-muted">{toolSubject(active)}</span>}
      </>}
      trailing={active ? <Duration item={active} live /> : null}
      expandable
    >
      <ul className="m-0 grid list-none gap-0.5 p-0">
        {items.map((item) => <ToolRow key={item.id} item={item} />)}
      </ul>
    </ActivityRow>
  );
}

/** The turn's steps in order; runs of commands and of reads fold into groups. */
export function ActivitySteps({ items, current }: { items: ChatItem[]; current: boolean }) {
  return <>{groupSteps(items).map((group) => {
    const key = group.items[0].id;
    if (group.kind === "commands") return <CommandGroup key={`group:${key}`} items={group.items} current={current} />;
    if (group.kind === "explore") return <ExploreGroup key={`group:${key}`} items={group.items} />;
    return <StepRow key={key} item={group.items[0]} />;
  })}</>;
}
