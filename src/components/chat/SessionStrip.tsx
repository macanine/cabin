import { Button } from "@mantine/core";
import { IconGitBranch } from "@tabler/icons-react";
import { useState } from "react";
import type { AgentKind, ChatAsyncTask, ChatPlanEntry, Session, SubagentTask } from "../../services/ipc/generated/core";
import type { TimelineRow } from "../../lib/chatTimeline";
import { sessionChanges } from "../../lib/chatTimeline";
import { toIpcError } from "../../services/ipc/common";
import { libraryIpc } from "../../services/ipc/library";
import { invalidateCatalogs } from "../../state/agentCatalog";
import { uiActions } from "../../state/ui";
import { AgentPlan } from "./AgentControls";
import { ChangeSummary } from "./ChangeSummary";
import { SubagentStrip } from "./SubagentStrip";
import { AsyncTasks } from "./AsyncTasks";
import { PILL, StripPopover } from "./strip";

const branchName = (base: string) => base.replace(/^refs\/(heads|remotes)\//, "");

/** The session's worktree: one pill, and its details (and removal) on click. */
function WorktreePill({ session, agent }: { session: Session; agent: AgentKind }) {
  const [removing, setRemoving] = useState(false);
  const worktree = session.worktree;
  if (!worktree) return null;
  return (
    <StripPopover
      width={340}
      target={<button type="button" className={PILL} title={worktree.path}><IconGitBranch size={13} /> {branchName(worktree.base)}</button>}
    >
      <div className="grid gap-2 p-1 text-xs [overflow-wrap:anywhere]">
        <strong className="text-[13px]">Own worktree</strong>
        <span className="font-mono text-faint">{worktree.path}</span>
        <span className="text-muted">Based on {worktree.base}. It can be removed once the session is stopped, the worktree is clean and its commits are on a branch or tag; the session then uses the workspace again.</span>
        <Button size="compact-xs" variant="default" className="self-start" loading={removing} onClick={() => {
          setRemoving(true);
          libraryIpc.removeWorktree(session.id)
            .then(() => invalidateCatalogs(agent))
            .catch((error) => uiActions.toast(toIpcError(error).message, "error"))
            .finally(() => setRemoving(false));
        }}>Remove worktree</Button>
      </div>
    </StripPopover>
  );
}

/** One quiet row of what the session is carrying: plan, changes, subagents,
 * background tasks, worktree. */
export function SessionStrip({ plan, rows, subagents, tasks, onStopTask, now, session, agent, open, onOpenChange, onSelect }: {
  plan: ChatPlanEntry[];
  rows: TimelineRow[];
  subagents: SubagentTask[];
  tasks: ChatAsyncTask[];
  onStopTask: (taskId: string) => Promise<boolean>;
  now: number;
  session: Session | null;
  agent: AgentKind;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelect: (id: string) => void;
}) {
  return (
    <div className="mb-1.5 flex flex-wrap items-start gap-1.5 empty:hidden">
      <AgentPlan plan={plan} />
      <ChangeSummary changes={sessionChanges(rows)} />
      <SubagentStrip tasks={subagents} now={now} open={open} onOpenChange={onOpenChange} onSelect={onSelect} />
      <AsyncTasks tasks={tasks} now={now} onStop={onStopTask} />
      {session && <WorktreePill session={session} agent={agent} />}
    </div>
  );
}
