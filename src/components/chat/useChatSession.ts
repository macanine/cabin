import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { useEffect, useRef, useState } from "react";
import type {
  ChatAsyncTask, ChatControls, ChatItem, ChatPermissionMode, ChatState, ChatTurnUsage, PlanReview, SubagentTask,
} from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { libraryIpc } from "../../services/ipc/library";
import { mergeUsage } from "../../lib/chatStats";
import { mergePlan, mergePlans, mergeSubagents } from "../../lib/chatPlans";
import { EMPTY_CONTROLS } from "../../lib/agentControls";
import { onChatEvent, useApp } from "../../state/store";
import { uiActions } from "../../state/ui";
import { DEFAULT_CHAT_PERMISSION } from "../PermissionPicker";

function mergeItems(current: ChatItem[], incoming: ChatItem[]): ChatItem[] {
  const byId = new Map(current.map((item) => [item.id, item]));
  for (const item of incoming) if ((byId.get(item.id)?.revision ?? 0) < item.revision) byId.set(item.id, item);
  return [...byId.values()].sort((a, b) => a.ordinal - b.ordinal);
}

function mergeTask(current: ChatAsyncTask[], incoming: ChatAsyncTask): ChatAsyncTask[] {
  const index = current.findIndex((task) => task.id === incoming.id);
  if (index < 0) return [...current, incoming];
  if (current[index].updatedAt > incoming.updatedAt) return current;
  const next = [...current];
  next[index] = incoming;
  return next;
}

/**
 * One chat's live state: the snapshot from the guest, kept current by its
 * events. Rust is the source of truth; this only merges by revision.
 */
export function useChatSession(sessionId: string) {
  const guestReady = useApp((s) => s.guestReady);
  const [items, setItems] = useState<ChatItem[]>([]);
  const [state, setState] = useState<ChatState>({ state: "starting", message: null });
  const [model, setModelState] = useState<string | null>(null);
  const [permission, setPermission] = useState<ChatPermissionMode>(DEFAULT_CHAT_PERMISSION);
  const [plans, setPlans] = useState<PlanReview[]>([]);
  const [subagents, setSubagents] = useState<SubagentTask[]>([]);
  const [tasks, setTasks] = useState<ChatAsyncTask[]>([]);
  const [usage, setUsage] = useState<ChatTurnUsage[]>([]);
  const [controls, setControls] = useState<ChatControls>(EMPTY_CONTROLS);
  const modelVersion = useRef(0);
  const [snapshotFailed, setSnapshotFailed] = useState(false);
  const refreshRef = useRef<() => Promise<void>>(async () => undefined);

  useEffect(() => {
    let disposed = false;
    let stateVersion = 0;
    let reading = false;
    setSnapshotFailed(false);
    setItems([]); setPlans([]); setSubagents([]); setTasks([]); setUsage([]);
    setState({ state: "starting", message: null }); setModelState(null);
    setPermission(DEFAULT_CHAT_PERMISSION); setControls(EMPTY_CONTROLS);

    async function refresh() {
      if (disposed || reading) return;
      reading = true;
      const version = stateVersion;
      const modelAtStart = modelVersion.current;
      try {
        const snapshot = await libraryIpc.chatSnapshot(sessionId);
        if (disposed) return;
        setSnapshotFailed(false);
        setItems((old) => mergeItems(old, snapshot.items));
        setPlans((old) => mergePlans(old, snapshot.plans));
        setSubagents((old) => mergeSubagents(old, snapshot.subagents ?? []));
        // Older guest daemons predate async tasks.
        setTasks((old) => (snapshot.tasks ?? []).reduce(mergeTask, old));
        if (version === stateVersion) setState(snapshot.state);
        if (modelAtStart === modelVersion.current) setModelState(snapshot.model);
        setPermission(snapshot.permission);
        setUsage((old) => snapshot.usage.reduce(mergeUsage, old));
        setControls(snapshot.controls);
      } catch (error) {
        if (!disposed) {
          setSnapshotFailed(true);
          if (version === stateVersion) setState({ state: "error", message: toIpcError(error).message });
        }
      } finally { reading = false; }
    }
    refreshRef.current = refresh;
    const unsubscribe = onChatEvent(sessionId, (event) => {
      if (!event) { void refresh(); return; }
      switch (event.type) {
        case "chatItem": setItems((old) => mergeItems(old, [event.item])); break;
        case "chatPlan": setPlans((old) => mergePlan(old, event.plan)); break;
        case "chatSubagent": setSubagents((old) => mergeSubagents(old, [event.task])); break;
        case "chatTask": setTasks((old) => mergeTask(old, event.task)); break;
        case "chatState": stateVersion++; setState(event.state); break;
        case "chatModel": modelVersion.current++; setModelState(event.model); break;
        case "chatUsage": setUsage((old) => mergeUsage(old, event.turn)); break;
        case "chatControls": setControls(event.controls); break;
        case "chatPermission": setPermission(event.permission); break;
      }
    });
    if (guestReady) void refresh();
    return () => { disposed = true; unsubscribe(); };
  }, [sessionId, guestReady]);

  useAutoRefresh(() => refreshRef.current(), { enabled: guestReady && snapshotFailed, interval: 10_000 });

  // An error is announced once per distinct message.
  const shownError = useRef<string | null>(null);
  useEffect(() => {
    if (state.state !== "error") { shownError.current = null; return; }
    if (state.message && shownError.current !== state.message) {
      shownError.current = state.message;
      uiActions.toast(state.message, "error");
    }
  }, [state.state, state.message]);

  /** A model the user picked locally wins over a snapshot already in flight. */
  async function setModel(next: string) {
    await libraryIpc.chatSetModel(sessionId, next);
    modelVersion.current++;
    setModelState(next);
  }

  async function changePermission(next: ChatPermissionMode) {
    await libraryIpc.chatSetPermission(sessionId, next);
    setPermission(next);
  }

  return {
    items, state, model, permission, plans, setPlans, subagents, tasks, usage, controls,
    setModel, changePermission,
  };
}
