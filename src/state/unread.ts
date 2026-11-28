// Session attention for the task list.
// - Unread: a terminal printed, or a chat turn finished, after you last
//   looked. Both times persist per viewer (localStorage); a cleared storage
//   just means nothing is unread.
// - Activity: a chat's agent is working or waiting for you, from the
//   backend's chat state events.
import { useSyncExternalStore } from "react";
import type { ChatRunState } from "../services/ipc/generated/core";
import { uiActions } from "./ui";

const KEY = "cabin.unread";

interface Times {
  output: Record<string, number>;
  seen: Record<string, number>;
}

function read(): Times {
  try {
    const value = JSON.parse(localStorage.getItem(KEY) ?? "{}") as Partial<Times>;
    return { output: value.output ?? {}, seen: value.seen ?? {} };
  } catch {
    return { output: {}, seen: {} };
  }
}

const times = read();
let unread = compute();
const listeners = new Set<() => void>();
let saveTimer: number | undefined;

function compute(): Set<string> {
  return new Set(Object.keys(times.output).filter((id) => times.output[id] > (times.seen[id] ?? 0)));
}

function changed() {
  const next = compute();
  const same = next.size === unread.size && [...next].every((id) => unread.has(id));
  if (!same) {
    unread = next;
    listeners.forEach((listener) => listener());
  }
  window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    try {
      localStorage.setItem(KEY, JSON.stringify(times));
    } catch {
      // Not persisted in this viewer.
    }
  }, 1000);
}

function viewing(id: string): boolean {
  const view = uiActions.getView();
  return view.kind === "session" && view.id === id;
}

/** Output arrived for a session. */
export function noteOutput(id: string) {
  const now = Date.now();
  if (viewing(id)) {
    if (!unread.has(id) && now - (times.output[id] ?? 0) < 1000) return;
    times.seen[id] = now;
  }
  times.output[id] = now;
  changed();
}

/** The user is looking at this session now. */
export function markSeen(id: string) {
  times.seen[id] = Date.now();
  changed();
}

export function useUnread(): Set<string> {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => unread,
  );
}

export type Activity = "working" | "waiting";

let activity = new Map<string, Activity>();
const activityListeners = new Set<() => void>();

/** A chat changed state: track work, and mark the reply unread when a turn ends. */
export function noteChatState(id: string, state: ChatRunState) {
  const next: Activity | null = state === "running" ? "working" : state === "waiting" ? "waiting" : null;
  const previous = activity.get(id) ?? null;
  if (next === previous) return;
  activity = new Map(activity);
  if (next) activity.set(id, next);
  else activity.delete(id);
  activityListeners.forEach((listener) => listener());
  // A finished turn, or a request for you, is news.
  if ((previous === "working" && next === null && state === "ready") || next === "waiting") noteOutput(id);
}

export function useActivity(): ReadonlyMap<string, Activity> {
  return useSyncExternalStore(
    (listener) => {
      activityListeners.add(listener);
      return () => activityListeners.delete(listener);
    },
    () => activity,
  );
}
