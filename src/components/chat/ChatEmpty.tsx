import type { AgentKind } from "../../services/ipc/generated/core";
import { AgentLogo, AGENT_NAMES } from "../common";

/**
 * A new conversation: who is listening, where, and how to reach commands and
 * files. Nothing to read twice.
 */
export function ChatEmpty({ agent, workspace, path, starting }: {
  agent: AgentKind;
  workspace: string;
  path: string | null;
  starting: boolean;
}) {
  const kbd = "mx-0.5 inline-grid h-[18px] min-w-[18px] place-items-center rounded-[5px] border border-b-2 border-line bg-surface px-1 font-mono text-[11px] text-muted";
  return (
    <div className="flex min-h-[min(52vh,420px)] flex-col items-center justify-center gap-2 text-center text-muted">
      <span className="glass mb-1.5 grid place-items-center rounded-2xl p-3 text-accent"><AgentLogo agent={agent} size={40} /></span>
      <strong className="text-2xl font-semibold tracking-tight text-fg">{starting ? `Starting ${AGENT_NAMES[agent]}…` : `What should ${AGENT_NAMES[agent]} work on?`}</strong>
      <span className="max-w-[520px] truncate text-[13px]" title={path ?? undefined}>{workspace}{path ? <span className="font-mono text-xs text-faint"> · {path}</span> : null}</span>
      <span className="mt-2.5 text-xs text-faint">
        <kbd className={kbd}>/</kbd> commands and skills · <kbd className={kbd}>@</kbd> mention a file · <kbd className={kbd}>⇧</kbd><kbd className={kbd}>↵</kbd> new line
      </span>
    </div>
  );
}
