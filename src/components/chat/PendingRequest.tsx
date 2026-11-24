import { Button } from "@mantine/core";
import {
  IconArrowBackUp, IconCheck, IconInfoCircle, IconMessageQuestion,
  IconSend, IconShieldCheck, IconX,
} from "@tabler/icons-react";
import { useEffect, useRef, useState } from "react";
import type { ApprovalDecisionView, ChatItem, ChatQuestion } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { libraryIpc } from "../../services/ipc/library";
import { uiActions } from "../../state/ui";
import { FileChanges } from "./FileChanges";
import { Markdown } from "./Markdown";

/** Offered when an item carries no decision list; both are safe for every agent. */
const BASIC_DECISIONS: ApprovalDecisionView[] = [
  { decision: "decline", label: "Decline", warning: null },
  { decision: "accept", label: "Approve", warning: null },
];
/** Decisions are `accept`, `acceptAlways`, `decline`, … or `<one of those>:<option id>`
 * when an agent offers several options of one kind. */
const baseDecision = (decision: string) => decision.split(":")[0];
const isNegative = (decision: string) => ["decline", "declineAlways", "cancel"].includes(baseDecision(decision));
/** One key per decision, shown on its button. */
const KEYS: Record<string, string> = { accept: "y", acceptForSession: "s", acceptAlways: "a", decline: "n", declineAlways: "d" };

/** A key press that should answer the card: no modifier, and not typing into a field that has text. */
function answerKey(event: KeyboardEvent): string | null {
  if (event.metaKey || event.ctrlKey || event.altKey || event.isComposing) return null;
  const target = event.target as HTMLElement | null;
  if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) {
    if ((target as HTMLInputElement).value) return null;
  }
  return event.key.toLowerCase();
}

function Question({ question, value, onChange }: { question: ChatQuestion; value: string[]; onChange: (values: string[]) => void }) {
  const custom = value.filter((v) => !question.options.includes(v)).join(", ");
  return (
    <fieldset className="m-0 grid min-w-0 gap-2 border-0 p-0">
      <legend className="mb-1.5 p-0 text-[13px] font-medium">{question.question}</legend>
      {question.options.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {question.options.map((option) => {
            const checked = value.includes(option);
            return (
              <button
                key={option}
                type="button"
                className={`press cursor-pointer rounded-full border px-3 py-1 text-[13px] ${checked ? "border-accent bg-accent-soft font-medium text-accent shadow-[0_0_0_2px_var(--accent-soft)]" : "border-line bg-surface/70 text-fg hover:border-accent/50"}`}
                aria-pressed={checked}
                onClick={() => onChange(question.multiple ? (checked ? value.filter((v) => v !== option) : [...value, option]) : [option])}
              >
                {option}
              </button>
            );
          })}
        </div>
      )}
      <input
        className="w-full rounded-lg border border-line bg-surface/70 px-3 py-1.5 font-[inherit] text-[13px] text-fg transition-colors duration-[var(--motion-fast)] outline-none placeholder:text-faint focus:border-[color:var(--field-focus-border)]"
        aria-label={`Answer: ${question.question}`}
        placeholder={question.options.length ? "Or type another answer" : "Your answer"}
        value={custom}
        onChange={(event) => {
          const text = event.currentTarget.value;
          const chosen = question.multiple ? value.filter((v) => question.options.includes(v)) : [];
          onChange(text ? [...chosen, text] : chosen);
        }}
      />
    </fieldset>
  );
}

/** A decision's key, drawn as a small keycap on its button. */
const Key = ({ k }: { k: string }) => <kbd className="ml-0.5 rounded border border-b-2 border-line bg-surface px-1 font-mono text-[10px] text-muted">{k.toUpperCase()}</kbd>;

/** The oldest waiting approval or question, docked above the composer. */
export function PendingRequest({ sessionId, item }: { sessionId: string; item: ChatItem }) {
  const [answers, setAnswers] = useState<Record<string, string[]>>({});
  const [sending, setSending] = useState<string | null>(null);
  const [details, setDetails] = useState(false);
  const questions = item.questions;
  const isQuestion = questions.length > 0;
  const decisions = item.approval?.decisions ?? BASIC_DECISIONS;

  useEffect(() => {
    setAnswers({});
    setDetails(false);
  }, [item.id]);

  // One-key approvals: Y allows, N denies, A/S remember, as the buttons show.
  const respondRef = useRef<(decision: string) => void>(() => undefined);
  respondRef.current = (decision) => void respond(decision);
  useEffect(() => {
    if (isQuestion) return;
    function onKey(event: KeyboardEvent) {
      const key = answerKey(event);
      const decision = key && decisions.find((d) => KEYS[d.decision] === key);
      if (!decision) return;
      event.preventDefault();
      respondRef.current(decision.decision);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [item.id, isQuestion, decisions]);

  async function respond(decision: string) {
    if (!item.requestId || sending) return;
    setSending(decision);
    try {
      await libraryIpc.chatRespond(sessionId, item.requestId, decision, answers);
    } catch (error) {
      uiActions.toast(toIpcError(error).message, "error");
    } finally {
      setSending(null);
    }
  }

  const complete = questions.every((question) => (answers[question.id]?.length ?? 0) > 0);

  return (
    <div className="glass-strong mb-0 animate-fade rounded-t-[20px] rounded-b-none border-b-0 px-4 pt-3.5 pb-3" role="region" aria-label={isQuestion ? "Question from the agent" : "Approval needed"}>
      <div className="mb-2.5 flex items-center gap-2.5">
        <span className={`grid size-7 shrink-0 place-items-center rounded-lg ${isQuestion ? "bg-info/13 text-info" : "bg-warn/14 text-warn"}`}>
          {isQuestion ? <IconMessageQuestion size={16} /> : <IconShieldCheck size={16} />}
        </span>
        <strong className="min-w-0 flex-1 text-[13.5px] font-semibold">{item.title || (isQuestion ? "The agent has a question" : "Approval needed")}</strong>
        {item.detail && !isQuestion && (
          <button
            type="button"
            className="press grid size-6 cursor-pointer place-items-center rounded-md border-0 bg-transparent p-0 text-muted hover:bg-hover hover:text-fg"
            aria-label={details ? "Hide request details" : "Show request details"}
            title={details ? "Hide details" : "Details"}
            aria-expanded={details}
            onClick={() => setDetails((value) => !value)}
          >
            <IconInfoCircle size={15} />
          </button>
        )}
      </div>
      <div className="grid gap-2.5">
        {item.text && (item.approval?.kind === "plan"
          ? <div className="markdown selectable max-h-60 overflow-auto rounded-xl border border-line bg-surface/60 px-3.5 py-2.5"><Markdown text={item.text} /></div>
          : <pre className="selectable m-0 max-h-40 overflow-auto rounded-xl border border-line bg-surface/60 px-3 py-2 font-mono text-xs leading-relaxed whitespace-pre-wrap text-fg [overflow-wrap:anywhere]">{item.text}</pre>)}
        {item.changes.length > 0 && <FileChanges changes={item.changes} title="Proposed change" open />}
        {details && <pre className="selectable m-0 max-h-40 animate-fade overflow-auto rounded-xl bg-hover px-3 py-2 font-mono text-xs whitespace-pre-wrap text-muted [overflow-wrap:anywhere]">{item.detail}</pre>}
        {questions.map((question) => (
          <Question key={question.id} question={question} value={answers[question.id] ?? []} onChange={(value) => setAnswers((old) => ({ ...old, [question.id]: value }))} />
        ))}
      </div>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        {isQuestion ? (
          <>
            <Button size="xs" variant="default" leftSection={<IconArrowBackUp size={13} />} disabled={sending != null} loading={sending === "deny"} onClick={() => void respond("deny")}>Skip</Button>
            <span className="min-w-0 flex-1" />
            <Button size="xs" leftSection={<IconSend size={13} />} disabled={sending != null || !complete} loading={sending === "answer"} onClick={() => void respond("answer")}>Send answer</Button>
          </>
        ) : (
          <>
            {decisions.filter((d) => isNegative(d.decision)).map((d) => (
              <Button key={d.decision} size="xs" variant="default" leftSection={<IconX size={13} />} title={d.warning ?? undefined} disabled={sending != null} loading={sending === d.decision} onClick={() => void respond(d.decision)} rightSection={KEYS[d.decision] && <Key k={KEYS[d.decision]} />}>{d.label}</Button>
            ))}
            <span className="min-w-0 flex-1" />
            {decisions.filter((d) => !isNegative(d.decision)).map((d) => (
              <Button key={d.decision} size="xs" variant={d.decision === "accept" ? "filled" : "default"} leftSection={<IconCheck size={13} />} title={d.warning ?? undefined} disabled={sending != null} loading={sending === d.decision} onClick={() => void respond(d.decision)} rightSection={KEYS[d.decision] && <Key k={KEYS[d.decision]} />}>{d.label}</Button>
            ))}
          </>
        )}
      </div>
    </div>
  );
}
