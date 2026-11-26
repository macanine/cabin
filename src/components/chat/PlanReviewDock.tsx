import { IconArrowRight, IconCheck, IconClock, IconPencil } from "@tabler/icons-react";
import { useEffect, useRef, useState } from "react";
import type { PlanDecision, PlanReview, PlanRespondResult } from "../../services/ipc/generated/core";
import { libraryIpc } from "../../services/ipc/library";
import { toIpcError } from "../../services/ipc/common";
import { PLAN_STATUS_LABEL, planAcceptsDecision, planKey, planSummary } from "../../lib/chatPlans";
import { CopyPlan, PLAN_BUTTON, PLAN_PRIMARY, PlanConflictNotice, PlanTitle, ViewFullPlan } from "./PlanReviewCard";

const PERMISSION_LABEL: Record<PlanReview["execPermission"], string> = {
  "approval-required": "Supervised",
  "auto-accept-edits": "Auto-accept edits",
  auto: "Auto",
  plan: "Plan",
  "full-access": "Full access",
};

/** Approval dock. While it is mounted, sending is locked in Chat.tsx, so the
 * native waiting point cannot be stranded by an unrelated turn. */
export function PlanReviewDock({
  sessionId,
  plan,
  conflict = false,
  onPlan,
  onDismiss,
}: {
  sessionId: string;
  plan: PlanReview;
  conflict?: boolean;
  onPlan: (plan: PlanReview, outcome: PlanRespondResult["outcome"]) => void;
  onDismiss: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [held, setHeld] = useState<PlanReview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);
  // The plan's identity, not the object: a re-render with the same content
  // must not reset the editor.
  const key = planKey(plan);

  useEffect(() => {
    setBusy(false);
    setEditing(false);
    setFeedback("");
    setHeld(null);
    setError(null);
    root.current?.focus();
  }, [sessionId, key]);

  useEffect(() => {
    if (conflict) {
      setEditing(false);
      setFeedback("");
    }
  }, [conflict]);

  const shown = held ?? plan;
  const readOnlyConflict = conflict || held != null;
  const canDecide = !readOnlyConflict && planAcceptsDecision(plan) && !busy;
  const canApprove = canDecide && plan.complete;

  async function submit(decision: PlanDecision, text?: string) {
    if (!canDecide) return;
    if (decision === "request_changes" && !text?.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const result: PlanRespondResult = await libraryIpc.chatPlanRespond(
        sessionId,
        plan.planId,
        plan.revision,
        decision,
        text?.trim(),
      );
      onPlan(result.review, result.outcome);
      if (result.outcome === "conflict") {
        setHeld(result.review);
      } else {
        setEditing(false);
        setFeedback("");
      }
    } catch (cause) {
      // The backend reverts the submission and emits the current record. Keep
      // the dock open so the user can retry instead of showing a false success.
      setError(toIpcError(cause).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      ref={root}
      tabIndex={-1}
      className="grid animate-fade gap-2.5 border-b border-line px-4 pt-3.5 pb-3 outline-none"
      role="group"
      aria-label="Plan awaiting your decision"
    >
      <PlanTitle plan={shown} icon={<IconPencil size={14} />} />
      {planSummary(shown.content) && <p className="m-0 text-[13px] leading-relaxed text-muted">{planSummary(shown.content, 220)}</p>}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-faint">
        <span>{PLAN_STATUS_LABEL[shown.status]}</span>
        <span>Execution permission: {PERMISSION_LABEL[shown.execPermission]}</span>
        {shown.status === "submitting" && <span role="status" className="text-accent">Sending your decision…</span>}
      </div>
      <div className="flex flex-wrap gap-1.5">
        <ViewFullPlan plan={shown} />
        <CopyPlan content={shown.content} />
      </div>
      {readOnlyConflict && (
        <PlanConflictNotice>
          This plan changed before your decision reached the agent. The panel is now read-only; review revision {shown.revision} in the timeline.
          <button type="button" className={`${PLAN_BUTTON} ml-2`} onClick={onDismiss}>Dismiss</button>
        </PlanConflictNotice>
      )}
      {error && <PlanConflictNotice tone="bad">Could not apply the decision: {error}</PlanConflictNotice>}
      {editing && !readOnlyConflict && (
        <textarea
          className="w-full resize-none rounded-xl border border-line bg-surface/70 px-3 py-2 font-[inherit] text-[13px] text-fg transition-colors duration-[var(--motion-fast)] outline-none placeholder:text-faint focus:border-[color:var(--field-focus-border)]"
          value={feedback}
          onChange={(event) => setFeedback(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && (event.metaKey || event.ctrlKey) && !event.nativeEvent.isComposing) {
              event.preventDefault();
              void submit("request_changes", feedback);
            }
          }}
          rows={3}
          autoFocus
          aria-label="Feedback for plan changes"
          placeholder="Explain what should change. The agent will keep planning with this feedback."
        />
      )}
      <div className="flex flex-wrap items-center justify-end gap-2">
        {readOnlyConflict ? null : editing ? (
          <>
            <button type="button" className={PLAN_BUTTON} disabled={busy} onClick={() => { setEditing(false); setFeedback(""); }}>Cancel</button>
            <button type="button" className={PLAN_PRIMARY} disabled={!canDecide || !feedback.trim()} onClick={() => void submit("request_changes", feedback)}><IconArrowRight size={13} />Send feedback</button>
          </>
        ) : (
          <>
            <button type="button" className={PLAN_BUTTON} disabled={!canDecide} onClick={() => void submit("defer")}><IconClock size={13} />Defer</button>
            <button type="button" className={PLAN_BUTTON} disabled={!canDecide} onClick={() => setEditing(true)}>Request changes</button>
            <button type="button" className={PLAN_PRIMARY} disabled={!canApprove} title={plan.complete ? undefined : "The plan is still a draft"} onClick={() => void submit("approve")}><IconCheck size={13} />Approve and execute</button>
          </>
        )}
      </div>
    </div>
  );
}
