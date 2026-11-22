import {
  IconAlertCircle, IconCheck, IconClipboardText, IconCopy, IconEye, IconRefresh,
} from "@tabler/icons-react";
import { Modal } from "@mantine/core";
import { useMemo, useState } from "react";
import type { PlanReview, PlanReviewStatus } from "../../services/ipc/generated/core";
import { clipboard } from "../../services/ipc/dialogs";
import {
  PLAN_EXECUTION_LABEL, PLAN_STATUS_LABEL, isPlanTerminal, planAcceptsDecision, planKey, planSummary,
} from "../../lib/chatPlans";
import { Markdown } from "./Markdown";

const STATUS_TONE: Record<PlanReviewStatus, string> = {
  draft: "bg-hover text-muted",
  awaiting_review: "bg-accent-soft text-accent",
  submitting: "bg-accent-soft text-accent",
  approved: "bg-ok/13 text-ok",
  changes_requested: "bg-warn/14 text-warn",
  deferred: "bg-hover text-muted",
  cancelled: "bg-hover text-faint",
  expired: "bg-warn/14 text-warn",
  superseded: "bg-hover text-faint",
};

/** Small buttons shared by the plan cards and the dock. */
export const PLAN_BUTTON =
  "press inline-flex cursor-pointer items-center gap-1.5 rounded-lg border border-line bg-surface/70 px-2.5 py-1 text-xs font-medium text-fg hover:bg-surface disabled:cursor-default disabled:opacity-45";
export const PLAN_PRIMARY =
  "press inline-flex cursor-pointer items-center gap-1.5 rounded-lg border border-transparent bg-accent px-3 py-1 text-xs font-medium text-white shadow-[0_2px_8px_-3px_var(--accent),inset_0_1px_0_rgb(255_255_255/0.25)] hover:brightness-110 disabled:cursor-default disabled:opacity-45 disabled:shadow-none";

export function PlanStatusBadge({ status }: { status: PlanReviewStatus }) {
  return <span className={`shrink-0 rounded-full px-2 py-px text-[11px] font-medium ${STATUS_TONE[status]}`}>{PLAN_STATUS_LABEL[status]}</span>;
}

export const AgentTag = ({ agent }: { agent: PlanReview["agent"] }) => <span className="shrink-0 rounded bg-hover px-1.5 py-px font-mono text-[10.5px] text-muted">{agent}</span>;

export function CopyPlan({ content }: { content: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className={PLAN_BUTTON}
      onClick={() => {
        void clipboard.write(content).then(() => {
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1500);
        }).catch(() => undefined);
      }}
    >
      {copied ? <IconCheck size={13} className="animate-fade text-ok" /> : <IconCopy size={13} />}
      {copied ? "Copied" : "Copy"}
    </button>
  );
}

/** The plan's heading line: icon, title, agent, revision and status. */
export function PlanTitle({ plan, icon, extra }: { plan: PlanReview; icon: React.ReactNode; extra?: React.ReactNode }) {
  return (
    <header className="flex min-w-0 flex-wrap items-center gap-2 text-[13px]">
      <span className="grid size-6 shrink-0 place-items-center rounded-md bg-accent-soft text-accent">{icon}</span>
      <strong className="min-w-0 truncate text-[13.5px] font-semibold">{plan.title || "Plan"}</strong>
      <AgentTag agent={plan.agent} />
      <span className="text-xs text-faint">Revision {plan.revision}</span>
      <PlanStatusBadge status={plan.status} />
      {extra}
    </header>
  );
}

function PlanPreview({ plan, onClose }: { plan: PlanReview; onClose: () => void }) {
  return (
    <Modal
      opened
      onClose={onClose}
      size="xl"
      title={<span className="flex items-center gap-2"><IconClipboardText size={16} />{plan.title || "Plan"}<AgentTag agent={plan.agent} /><PlanStatusBadge status={plan.status} /></span>}
    >
      <div className="markdown selectable max-h-[65vh] overflow-auto pr-1"><Markdown text={plan.content} /></div>
      <div className="mt-3 flex justify-end"><CopyPlan content={plan.content} /></div>
    </Modal>
  );
}

export function ViewFullPlan({ plan }: { plan: PlanReview }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" className={PLAN_BUTTON} onClick={() => setOpen(true)}><IconEye size={13} />View full plan</button>
      {open && <PlanPreview plan={plan} onClose={() => setOpen(false)} />}
    </>
  );
}

function executionCopy(plan: PlanReview): string | null {
  if (plan.status !== "approved") return null;
  return PLAN_EXECUTION_LABEL[plan.execution];
}

export function PlanReviewCard({ plan, onResume }: { plan: PlanReview; onResume?: (plan: PlanReview) => void }) {
  const summary = useMemo(() => planSummary(plan.content), [plan.content]);
  const execution = executionCopy(plan);
  return (
    <article className={`glass grid gap-2 rounded-2xl p-3.5 transition-opacity duration-300 ${isPlanTerminal(plan.status) ? "opacity-75 hover:opacity-100" : ""}`}>
      <PlanTitle plan={plan} icon={<IconClipboardText size={14} />} extra={execution && <span className="text-xs text-muted">Execution: {execution}</span>} />
      {summary && <p className="m-0 text-[13px] leading-relaxed text-muted">{summary}</p>}
      {plan.feedback && <p className="m-0 rounded-lg bg-warn/10 px-2.5 py-1.5 text-xs text-warn">Changes: {plan.feedback}</p>}
      <div className="flex flex-wrap gap-1.5">
        <ViewFullPlan plan={plan} />
        <CopyPlan content={plan.content} />
        {plan.status === "deferred" && onResume && (
          <button type="button" className={PLAN_BUTTON} onClick={() => onResume(plan)}><IconRefresh size={13} />Continue review</button>
        )}
      </div>
    </article>
  );
}

export function PlanHistory({ plans, onResume }: { plans: PlanReview[]; onResume?: (plan: PlanReview) => void }) {
  if (!plans.length) return null;
  return (
    <div className="mt-3 grid gap-2.5" aria-label="Plan history">
      {plans.map((plan) => (
        <PlanReviewCard key={planKey(plan)} plan={plan} onResume={planAcceptsDecision(plan) && plan.status === "deferred" ? onResume : undefined} />
      ))}
    </div>
  );
}

/** A tinted notice inside a plan card. */
export function PlanConflictNotice({ children, tone = "warn" }: { children: React.ReactNode; tone?: "warn" | "bad" }) {
  return (
    <div className={`flex animate-fade items-start gap-2 rounded-lg px-2.5 py-1.5 text-xs ${tone === "bad" ? "bg-bad/10 text-bad" : "bg-warn/10 text-warn"}`} role={tone === "bad" ? "alert" : "status"}>
      <IconAlertCircle size={14} className="mt-px shrink-0" />
      <span className="min-w-0 flex-1">{children}</span>
    </div>
  );
}
