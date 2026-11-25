import { IconArrowDown } from "@tabler/icons-react";
import { useMemo, useRef, useState, useEffect } from "react";
import type {
  AgentKind, ChatAttachmentInput, ChatItem, ChatState, PlanReview, PlanRespondOutcome,
} from "../services/ipc/generated/core";
import { toIpcError } from "../services/ipc/common";
import { libraryIpc } from "../services/ipc/library";
import { activeTurn, deriveTimeline, pendingRequest } from "../lib/chatTimeline";
import { toolCounts, usageByTurn } from "../lib/chatStats";
import { isTaskActive, tasksByItem } from "../lib/chatTasks";
import { loadDraft, saveDraft } from "../lib/chatDrafts";
import { useNow } from "../lib/useNow";
import { guestNow } from "../lib/clock";
import { actionablePlan, hasOpenPlan, mergePlan, planKey, visiblePlans } from "../lib/chatPlans";
import { liveModel, reasoningOption, sessionOptions } from "../lib/agentControls";
import { sessionCommandFor, type SessionCommand } from "../lib/sessionCommands";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";
import { useChatAttachments } from "./ChatAttachments";
import { PendingRequest } from "./chat/PendingRequest";
import { SelectionBubble } from "./chat/SelectionBubble";
import { TimelineRows } from "./chat/Timeline";
import { ChatContext, type ChatContextValue } from "./chat/context";
import { PlanHistory } from "./chat/PlanReviewCard";
import { PlanReviewDock } from "./chat/PlanReviewDock";
import { Suggestions } from "./chat/AgentControls";
import { SubagentConversation } from "./chat/SubagentConversation";
import { GoalCard } from "./chat/GoalCard";
import { ChatComposer } from "./chat/ChatComposer";
import { SessionCommandDialog } from "./chat/SessionCommandDialog";
import { AGENT_NAMES } from "./common";
import { ChatEmpty } from "./chat/ChatEmpty";
import { PromptRail } from "./chat/PromptRail";
import { SessionStrip } from "./chat/SessionStrip";
import { useChatSession } from "./chat/useChatSession";
import { useAutoTitle } from "./chat/useAutoTitle";
import { useComposerMenu } from "./chat/useComposerMenu";
import { useFollowScroll } from "./chat/useFollowScroll";
import { useMessageQueue } from "./chat/useMessageQueue";
import { MENTION_EVENT, type MentionDetail } from "./workbench/mention";

function placeholder(state: ChatState["state"], waiting: boolean, planLocked: boolean, hasModel: boolean, empty: boolean): string {
  if (waiting) return "Answer the request above";
  if (planLocked) return "Review the plan above";
  if (state === "running") return "Queue a follow-up";
  const idle = state === "ready" || state === "stopped" || state === "error";
  if (!hasModel && idle) return "Choose a model to start";
  switch (state) {
    case "starting": return "Connecting…";
    case "switching": return "Switching model…";
    default: return empty ? "Describe a task" : "Reply";
  }
}

/** Put the caret at `position` (default: the end) on the next frame. */
function focusAt(input: HTMLTextAreaElement | null, position?: number) {
  requestAnimationFrame(() => {
    if (!input) return;
    const at = position ?? input.value.length;
    input.focus();
    input.setSelectionRange(at, at);
  });
}

/**
 * One agent conversation: the transcript, what the session is carrying, and
 * the composer. State comes from the guest (`useChatSession`); this view
 * only arranges it and sends what the user types.
 */
export function Chat({ sessionId, workspaceId, agent }: { sessionId: string; workspaceId: string; agent: AgentKind }) {
  const chat = useChatSession(sessionId);
  const { items, state, model, permission, plans, setPlans, subagents, tasks, usage, controls } = chat;
  const session = useApp((s) => s.sessions.find((entry) => entry.id === sessionId) ?? null);
  const workspace = useApp((s) => s.workspaces.find((entry) => entry.id === workspaceId) ?? null);
  useAutoTitle(sessionId, session, items, controls.title);

  const [resumedPlan, setResumedPlan] = useState<string | null>(null);
  const [conflictPlan, setConflictPlan] = useState<PlanReview | null>(null);
  const [subagentOpen, setSubagentOpen] = useState(false);
  const [subagentSelected, setSubagentSelected] = useState<string | null>(null);
  const [text, setText] = useState(() => loadDraft(sessionId));
  const [caret, setCaret] = useState(0);
  const [commandDialog, setCommandDialog] = useState<{ command: SessionCommand; opened: boolean } | null>(null);
  const attachments = useChatAttachments();
  const input = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    setResumedPlan(null); setConflictPlan(null); setSubagentOpen(false); setSubagentSelected(null);
  }, [sessionId]);
  useEffect(() => {
    if (resumedPlan && !plans.some((plan) => planKey(plan) === resumedPlan && plan.status === "deferred")) setResumedPlan(null);
  }, [plans, resumedPlan]);

  // The side panel's "Mention in chat" appends an @file to the composer.
  useEffect(() => {
    const onMention = (event: Event) => {
      const detail = (event as CustomEvent<MentionDetail>).detail;
      if (detail.sessionId !== sessionId) return;
      setText((old) => `${old && !old.endsWith(" ") && !old.endsWith("\n") ? `${old} ` : old}${detail.text}`);
      focusAt(input.current);
    };
    window.addEventListener(MENTION_EVENT, onMention);
    return () => window.removeEventListener(MENTION_EVENT, onMention);
  }, [sessionId]);

  // The unsent message is kept per session, so switching tasks never loses it.
  const draft = useRef(text);
  draft.current = text;
  useEffect(() => {
    const timer = setTimeout(() => saveDraft(sessionId, text), 300);
    return () => clearTimeout(timer);
  }, [sessionId, text]);
  useEffect(() => () => saveDraft(sessionId, draft.current), [sessionId]);

  const rows = useMemo(() => deriveTimeline({ items, state: state.state }), [items, state.state]);
  const pending = useMemo(() => pendingRequest(items), [items]);
  const turn = useMemo(() => {
    const active = activeTurn(items, state.state);
    return active && pending ? { ...active, waiting: true } : active;
  }, [items, state.state, pending]);
  const dockPlan = conflictPlan ?? actionablePlan(plans, resumedPlan);
  const planLocked = hasOpenPlan(plans) || conflictPlan != null;
  // The plan under review is in the dock; the transcript lists the others.
  const dockKey = dockPlan ? planKey(dockPlan) : null;
  const planHistory = useMemo(() => visiblePlans(plans).filter((plan) => planKey(plan) !== dockKey), [plans, dockKey]);

  // Answering a request gives the keyboard back to the message box.
  const answered = useRef(false);
  useEffect(() => {
    if (answered.current && !pending && (document.activeElement === document.body || document.activeElement == null)) input.current?.focus();
    answered.current = pending != null;
  }, [pending]);

  const scroll = useFollowScroll([items, state, pending, plans, subagents, dockPlan], rows.length);

  const live = useMemo(() => liveModel(controls), [controls]);
  const options = useMemo(() => sessionOptions(controls), [controls]);
  const reasoning = useMemo(() => reasoningOption(options), [options]);
  const otherOptions = useMemo(() => options.filter((option) => option !== reasoning), [options, reasoning]);
  const busy = state.state === "running" || state.state === "waiting";
  const idle = state.state === "ready" || state.state === "stopped" || state.state === "error";
  const activeSubagents = subagents.filter((task) => task.state === "active").length;
  const activeTasks = tasks.filter(isTaskActive).length;
  // Timers tick while anything runs: the turn, a subagent or background work.
  const now = useNow(busy || activeSubagents > 0 || activeTasks > 0, guestNow);
  const tools = useMemo(() => toolCounts(items), [items]);
  const usageMap = useMemo(() => usageByTurn(usage), [usage]);
  const subagentMap = useMemo(() => new Map(subagents.map((task) => [task.id, task])), [subagents]);
  const taskMap = useMemo(() => tasksByItem(tasks), [tasks]);
  const selectedSubagent = subagents.find((task) => task.id === subagentSelected) ?? null;
  const hasModel = model != null || live != null;

  const queue = useMessageQueue({
    sessionId, state: state.state, blocked: pending != null || planLocked, onSent: scroll.resume,
  });
  const hasAttachments = attachments.loading || attachments.files.length > 0;
  const command = sessionCommandFor(text, hasAttachments);
  const menu = useComposerMenu({
    text, caret, workspaceId, agent, sessionId, worktree: session?.worktree?.path ?? null, controls,
    includeSessionCommands: !hasAttachments,
    onApply: (next, at) => { setText(next); setCaret(at); focusAt(input.current, at); },
  });

  function clearComposer() {
    setText("");
    setCaret(0);
    draft.current = "";
    saveDraft(sessionId, "");
  }

  function interrupt() {
    void libraryIpc.chatInterrupt(sessionId).catch((error) => uiActions.toast(toIpcError(error).message, "error"));
  }

  async function send() {
    if (command) {
      setCommandDialog({ command, opened: true });
      clearComposer();
      return;
    }
    const message = text.trim();
    if (!canSend) return;
    if (busy) {
      // While the agent works, a send waits in the queue.
      if (attachments.files.length) { uiActions.toast("Attachments can be sent when the agent is idle", "info"); return; }
      queue.add(message);
      clearComposer();
      return;
    }
    // A stopped chat is idle: sending reconnects its agent and resumes the conversation.
    const finish = queue.startDirect();
    if (!finish) return;
    try {
      await libraryIpc.chatSend(sessionId, message, attachments.files);
      clearComposer();
      attachments.clear();
      scroll.resume();
    } catch (error) {
      uiActions.toast(toIpcError(error).message, "error");
    } finally {
      finish();
    }
  }

  function addImages(files: File[]) {
    if (!files.length) return;
    if (!controls.capabilities.prompt.images) { uiActions.toast("This agent does not accept images", "info"); return; }
    void attachments.add(files);
  }

  /** Quote a transcript selection into the composer as a Markdown block. */
  function quoteSelection(selected: string) {
    const quoted = selected.split("\n").map((line) => `> ${line}`).join("\n");
    setText((old) => `${old.trimEnd() ? `${old.trimEnd()}\n\n` : ""}${quoted}\n\n`);
    requestAnimationFrame(() => input.current?.focus());
  }

  /** Put an earlier prompt (and its attachments) back into the composer. */
  async function editTurn(turn: string) {
    const item = items.find((entry) => entry.id === turn && entry.kind === "user_message");
    if (!item) return;
    setText(item.text);
    focusAt(input.current);
    try {
      attachments.replace(item.attachments.length ? await savedAttachments(item) : []);
    } catch (error) {
      uiActions.toast(`Attachments could not be restored: ${toIpcError(error).message}`, "error");
    }
  }

  async function savedAttachments(item: ChatItem): Promise<ChatAttachmentInput[]> {
    return Promise.all(item.attachments.map(async (file) => {
      const data = await libraryIpc.chatAttachment(sessionId, file.id);
      return { name: file.name, mimeType: data.mimeType, base64: data.base64 };
    }));
  }

  async function stopTask(taskId: string): Promise<boolean> {
    try {
      const stopped = await libraryIpc.chatStopTask(sessionId, taskId);
      if (!stopped) uiActions.toast("The agent did not stop that task", "info");
      return stopped;
    } catch (error) {
      uiActions.toast(toIpcError(error).message, "error");
      return false;
    }
  }

  function applyPlanDecision(review: PlanReview, outcome: PlanRespondOutcome) {
    setPlans((old) => mergePlan(old, review));
    setConflictPlan(outcome === "conflict" ? review : null);
    if (outcome !== "conflict" && review.status !== "deferred") setResumedPlan(null);
  }

  const context: ChatContextValue = useMemo(() => ({
    sessionId,
    usage: usageMap,
    tools: tools.byTurn,
    subagents: subagentMap,
    tasks: taskMap,
    openSubagent: (taskId: string) => { setSubagentOpen(false); setSubagentSelected(taskId); },
    edit: (turn: string) => void editTurn(turn),
    now,
  }), [sessionId, usageMap, tools, subagentMap, taskMap, now, items]); // eslint-disable-line react-hooks/exhaustive-deps

  const canSend = (text.trim().length > 0 || attachments.files.length > 0) && !attachments.loading
    && (idle || busy) && hasModel && !pending && !planLocked && !queue.sending;

  return (
    <ChatContext.Provider value={context}>
      <div className="chat-view relative flex min-h-0 min-w-0 flex-1 flex-row bg-bg">
        <div className="relative flex min-h-0 min-w-0 flex-1 flex-col">
          <div ref={scroll.ref} className="chat-scroll" onScroll={scroll.onScroll}>
            <div className="chat-timeline" aria-live="polite">
              {items.length === 0 && (
                <ChatEmpty agent={agent} workspace={workspace?.name ?? "Workspace"} path={session?.worktree?.path ?? workspace?.path ?? null} starting={state.state === "starting"} />
              )}
              <TimelineRows rows={rows} waiting={state.state === "waiting"} />
              <PlanHistory plans={planHistory} onResume={(plan) => setResumedPlan(planKey(plan))} />
              {state.state === "error" && state.message && (
                <div className="mt-3 flex animate-fade items-center gap-3 rounded-xl border border-bad/30 bg-bad/8 px-3.5 py-2.5 text-sm text-bad" role="status">
                  <span className="grid min-w-0 flex-1 gap-0.5 [overflow-wrap:anywhere]">{state.message}<span className="text-xs text-muted">Your conversation is saved. Send a message to reconnect the agent.</span></span>
                </div>
              )}
            </div>
          </div>
          <PromptRail rows={rows} scrollRef={scroll.ref} />
          <SelectionBubble containerRef={scroll.ref} onQuote={quoteSelection} />
          <div className="relative mx-auto w-[min(100%,792px)] px-6 pt-2 pb-4 before:pointer-events-none before:absolute before:inset-x-0 before:-top-7 before:h-7 before:bg-linear-to-t before:from-bg before:to-transparent">
            {!scroll.atEnd && (
              <button
                className={`press glass-strong absolute -top-11 left-1/2 z-10 inline-flex h-8 -translate-x-1/2 cursor-pointer items-center gap-1.5 rounded-full px-3 text-xs ${scroll.unseen ? "!border-accent/40 text-accent" : "text-muted hover:text-fg"}`}
                onClick={scroll.jump}
                aria-label="Jump to the latest message"
              >
                <IconArrowDown size={14} />
                {scroll.unseen > 0 && <span className="font-medium">New messages</span>}
              </button>
            )}
            {idle && !pending && !text && (
              <Suggestions items={controls.suggestions} onPick={(next) => { setText(next); input.current?.focus(); }} />
            )}
            <div className="chat-dock" data-chat-dock>
              {controls.goal && (
                <GoalCard goal={controls.goal} onControl={(action) => {
                  void libraryIpc.chatGoalControl(sessionId, action).catch((error) => uiActions.toast(toIpcError(error).message, "error"));
                }} />
              )}
              <SessionStrip
                turn={turn}
                plan={controls.plan} rows={rows} subagents={subagents} tasks={tasks} onStopTask={stopTask} now={now} session={session} agent={agent}
                open={subagentOpen} onOpenChange={setSubagentOpen}
                onSelect={(id) => { setSubagentOpen(false); setSubagentSelected(id); }}
              />
              {dockPlan && (
                <PlanReviewDock sessionId={sessionId} plan={dockPlan} conflict={conflictPlan != null} onPlan={applyPlanDecision} onDismiss={() => setConflictPlan(null)} />
              )}
              {pending && <PendingRequest sessionId={sessionId} item={pending} />}
              <ChatComposer
                input={input}
                text={text}
                onText={setText}
                onCaret={setCaret}
                placeholder={placeholder(state.state, pending != null, planLocked, hasModel, items.length === 0)}
                busy={busy}
                sending={queue.sending}
                commandLabel={command === "status" ? "Open session status" : command === "context" ? "Open context window" : null}
                canSend={canSend}
                canSteer={controls.canSteer && busy && !queue.sending && !pending && !planLocked}
                menu={menu}
                queue={{ ...queue, onEdit: (message) => { queue.remove(message); setText(message.text); focusAt(input.current); } }}
                attachments={attachments}
                onImages={addImages}
                onSend={() => void send()}
                onStop={interrupt}
                onRecall={() => [...items].reverse().find((item) => item.kind === "user_message" && item.text)?.text ?? null}
                agent={agent}
                workspaceId={workspaceId}
                model={model}
                live={live}
                reasoning={reasoning}
                otherOptions={otherOptions}
                optionsDisabled={state.state === "starting" || state.state === "stopped" || state.state === "error"}
                permission={permission}
                controls={controls}
                onModel={chat.setModel}
                onReasoning={reasoning ? (value) => libraryIpc.chatSetOption(sessionId, reasoning.id, value) : undefined}
                onPermission={chat.changePermission}
                onOption={(id, value) => libraryIpc.chatSetOption(sessionId, id, value)}
              />
            </div>
          </div>
        </div>
        {commandDialog && (
          <SessionCommandDialog
            command={commandDialog.command} opened={commandDialog.opened}
            onClose={() => setCommandDialog({ ...commandDialog, opened: false })}
            onExited={() => focusAt(input.current)}
            state={state} agentName={controls.agentName ?? AGENT_NAMES[agent]}
            model={live?.choices.find((choice) => choice.value === (model ?? live.current))?.name ?? model ?? live?.current ?? null}
            permission={permission} context={controls.context}
          />
        )}
        {selectedSubagent && (
          <SubagentConversation task={selectedSubagent} onQuote={quoteSelection} onClose={() => setSubagentSelected(null)} />
        )}
      </div>
    </ChatContext.Provider>
  );
}
