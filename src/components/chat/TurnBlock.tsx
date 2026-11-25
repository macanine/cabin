import { useState, type ReactNode } from "react";
import type { ChatItem } from "../../services/ipc/generated/core";
import { elapsedLabel, isToolItem, type TimelineRow } from "../../lib/chatTimeline";
import { formatTokens, tokensPerSecond } from "../../lib/chatStats";
import { useChat } from "./context";
import { FileChanges } from "./FileChanges";
import { Markdown } from "./Markdown";
import { ActivitySteps } from "./ActivityRows";
import { TurnHeader } from "./ActivityRow";
import { CopyMenu, EditButton } from "./RowControls";
import { Reveal } from "../motion";

/** A completed turn's muted figure line: duration, steps, tokens, speed. */
function TurnMeta({ row }: { row: Extract<TimelineRow, { kind: "turn" }> }) {
  const { usage, tools } = useChat();
  const turnUsage = usage.get(row.turn);
  const toolCount = tools.get(row.turn) ?? 0;
  const speed = turnUsage ? tokensPerSecond(turnUsage) : null;
  const answer = row.items.find((item) => item.id === row.answerId);
  const parts = [
    row.elapsed != null && row.elapsed > 0 ? elapsedLabel(row.elapsed) : null,
    toolCount ? `${toolCount} ${toolCount === 1 ? "step" : "steps"}` : null,
    turnUsage && turnUsage.output > 0 ? `${formatTokens(turnUsage.input + turnUsage.output)} tokens` : null,
    speed != null ? `${speed < 10 ? speed.toFixed(1) : Math.round(speed)} tok/s` : null,
  ].filter(Boolean);
  return (
    <div className="group/meta flex min-h-6 items-center justify-between gap-3 text-xs text-faint">
      {parts.length > 0 && <span className="tabular-nums">{parts.join(" · ")}</span>}
      <span className="ml-auto flex gap-px opacity-0 transition-opacity duration-200 group-hover/meta:opacity-100 focus-within:opacity-100">
        <EditButton turn={row.turn} />
        {answer?.text && <CopyMenu markdown={answer.text} />}
      </span>
    </div>
  );
}

function itemRunning(items: ChatItem[]): boolean {
  return items.some((item) =>
    item.status === "running" || item.status === "pending" || item.status === "waiting",
  );
}

/**
 * One assistant reply: its activity capsule, the process on a left rail, the
 * answer under it, and the changed files it left behind. The process folds
 * away as soon as a newer prompt arrives; the answer stays.
 */
export function TurnBlock({
  row,
  waiting,
  foldEpoch,
}: {
  row: Extract<TimelineRow, { kind: "turn" }>;
  waiting: boolean;
  /** Bumped by every user send: folds whatever was opened above it. */
  foldEpoch: number;
}) {
  const answer = row.answerId
    ? row.items.find((item) => item.id === row.answerId) ?? null
    : null;
  const progress = row.items.filter((item) => item.id !== row.answerId);
  const hasAnswer = Boolean(answer?.text.trim());
  // A settled reply with no answer left to keep stays open: folding it would
  // leave a lone capsule, hiding the only content it has (a turn stopped on a
  // tool call, or one whose card is the answer).
  const foldable = progress.some((item) => item.id !== row.answerId)
    && (row.current || hasAnswer);
  const [manual, setManual] = useState<{ epoch: number; open: boolean } | null>(null);
  // Completed turns start folded; only the turn being written keeps its steps
  // open. The answer itself always stays below the fold.
  const open = foldable
    ? (manual?.epoch === foldEpoch ? manual.open : row.current)
    : true;

  const stepCount = progress.filter((item) => isToolItem(item) || item.kind === "reasoning").length;
  const failures = progress.filter((item) => item.status === "failed" || item.kind === "error").length;
  const working = row.current && !waiting && itemRunning(row.items);
  const meta: ReactNode[] = [
    stepCount > 0 ? `${stepCount} ${stepCount === 1 ? "step" : "steps"}` : null,
    failures > 0 ? <span className="text-bad">{failures} failed</span> : null,
  ].filter((value) => value != null);

  return (
    <div className="grid min-w-0 gap-3">
      {foldable && <TurnHeader meta={meta} open={open} working={working} onToggle={() => setManual({ epoch: foldEpoch, open: !open })} />}
      {progress.length > 0 && (
        <Reveal open={foldable ? open : true} className="-mx-1.5 -mt-1">
          <ol className="m-0 grid list-none gap-0.5 px-1.5 pt-1 pb-1">
            <ActivitySteps items={progress} current={row.current} />
          </ol>
        </Reveal>
      )}
      {answer?.text && (
        <div className="turn-answer markdown">
          <Markdown text={answer.text} streaming={working} />
        </div>
      )}
      {row.changes.length > 0 && <FileChanges changes={row.changes} />}
      {!row.current && <TurnMeta row={row} />}
    </div>
  );
}
