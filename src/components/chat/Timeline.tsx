import { useLayoutEffect, useMemo, useRef } from "react";
import type { ActiveTurn, TimelineRow } from "../../lib/chatTimeline";
import { subagentsByTurn } from "../../lib/chatTasks";
import { useChat } from "./context";
import { TurnBlock } from "./TurnBlock";
import { UserMessage } from "./UserMessage";

/** Fade in newly sent messages. History is visible immediately. */
export function TimelineRows({
  rows,
  waiting = false,
  activeTurn = null,
}: {
  rows: TimelineRow[];
  waiting?: boolean;
  activeTurn?: ActiveTurn | null;
}) {
  const { subagents } = useChat();
  const seen = useRef<Set<string> | null>(null);
  const fresh = new Set<string>();
  if (seen.current) for (const row of rows) if (!seen.current.has(row.id)) fresh.add(row.id);
  useLayoutEffect(() => {
    if (!rows.length) return;
    seen.current ??= new Set();
    for (const row of rows) seen.current.add(row.id);
  });
  const foldEpoch = useMemo(
    () => rows.filter((row) => row.kind === "user").length,
    [rows],
  );
  const subagentsMap = useMemo(
    () => subagentsByTurn(rows, [...subagents.values()]),
    [rows, subagents],
  );
  return (
    <>
      {rows.map((row) => (
        <div
          key={row.id}
          className={`flex min-w-0 flex-col ${row.kind === "user" ? "mt-7 first:mt-0" : "mt-2.5"} ${row.kind === "user" && fresh.has(row.id) ? "chat-msg-enter" : ""}`}
          data-turn-anchor={row.kind === "user" ? row.id : undefined}
        >
          {row.kind === "user"
            ? <UserMessage row={row} />
            : (
              <TurnBlock
                row={row}
                waiting={waiting}
                foldEpoch={foldEpoch}
                liveTurn={row.current ? activeTurn : null}
                turnSubagents={subagentsMap.get(row.turn) ?? []}
              />
            )}
        </div>
      ))}
    </>
  );
}
