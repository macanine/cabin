import { queueMessage, type QueuedMessage } from "./agentControls.ts";

interface QueueState {
  messages: QueuedMessage[];
  active: string | symbol | null;
  failed: string[];
}

const empty: QueueState = { messages: [], active: null, failed: [] };

const KEY = "cabin.chatQueue";
/** Sessions whose queues are kept; the oldest go first. */
export const QUEUE_SESSION_LIMIT = 30;

/**
 * Queues saved by an earlier run. Every message waits for an explicit Retry:
 * the one being sent may have arrived, and the rest were meant for a
 * conversation that may have moved on.
 */
export function restoreQueues(saved: string | null): Map<string, QueueState> {
  const restored = new Map<string, QueueState>();
  try {
    const value: unknown = JSON.parse(saved ?? "{}");
    if (!value || typeof value !== "object" || Array.isArray(value)) return restored;
    for (const [id, list] of Object.entries(value)) {
      if (!Array.isArray(list)) continue;
      const messages = list.filter((entry): entry is QueuedMessage =>
        entry != null && typeof entry.id === "string" && typeof entry.text === "string" && entry.text !== "")
        .map(({ id: messageId, text }) => ({ id: messageId, text }));
      if (messages.length) restored.set(id, { messages, active: null, failed: messages.map((message) => message.id) });
    }
  } catch {
    // Unreadable: start empty.
  }
  return restored;
}

function readSaved(): string | null {
  try { return localStorage.getItem(KEY); } catch { return null; }
}

const sessions = restoreQueues(readSaved());
const listeners = new Set<() => void>();

function get(sessionId: string): QueueState {
  return sessions.get(sessionId) ?? empty;
}

function set(sessionId: string, state: QueueState) {
  if (state.messages.length || state.active !== null) sessions.set(sessionId, state);
  else sessions.delete(sessionId);
  save();
  listeners.forEach((listener) => listener());
}

let saved = "";
function save() {
  const entries = [...sessions].filter(([, state]) => state.messages.length).slice(-QUEUE_SESSION_LIMIT);
  const next = JSON.stringify(Object.fromEntries(entries.map(([id, state]) => [id, state.messages])));
  if (next === saved) return;
  saved = next;
  try {
    if (entries.length) localStorage.setItem(KEY, next);
    else localStorage.removeItem(KEY);
  } catch {
    // Not kept in this viewer.
  }
}

function updateMessages(sessionId: string, change: (messages: QueuedMessage[]) => QueuedMessage[]) {
  const state = get(sessionId);
  const messages = change(state.messages);
  if (messages === state.messages) return;
  set(sessionId, { ...state, messages });
}

export const chatQueue = {
  get,
  subscribe(listener: () => void) {
    listeners.add(listener);
    return () => { listeners.delete(listener); };
  },
  add(sessionId: string, text: string) {
    updateMessages(sessionId, (messages) => queueMessage(messages, text));
  },
  /** Drop a deleted session's queue. */
  forget(sessionId: string) {
    if (sessions.has(sessionId)) set(sessionId, empty);
  },
  remove(sessionId: string, id: string) {
    const state = get(sessionId);
    if (state.active === id) return;
    set(sessionId, {
      ...state,
      messages: state.messages.filter((message) => message.id !== id),
      failed: state.failed.filter((failed) => failed !== id),
    });
  },
  move(sessionId: string, fromId: string, toId: string) {
    updateMessages(sessionId, (messages) => {
      const from = messages.findIndex(({ id }) => id === fromId);
      const to = messages.findIndex(({ id }) => id === toId);
      if (from < 0 || to < 0 || from === to) return messages;
      const next = [...messages];
      const [moved] = next.splice(from, 1);
      next.splice(to, 0, moved);
      return next;
    });
  },
  startDirect(sessionId: string) {
    const state = get(sessionId);
    if (state.active !== null) return null;
    const active = Symbol();
    set(sessionId, { ...state, active });
    return () => {
      const current = get(sessionId);
      if (current.active === active) set(sessionId, { ...current, active: null });
    };
  },
  start(sessionId: string, id: string, explicit = false) {
    const state = get(sessionId);
    if (state.active !== null) return false;
    if (!explicit && (state.failed.length > 0 || state.messages[0]?.id !== id)) return false;
    if (explicit && !state.messages.some((message) => message.id === id)) return false;
    set(sessionId, { ...state, active: id, failed: state.failed.filter((failed) => failed !== id) });
    return true;
  },
  finish(sessionId: string, id: string, delivered: boolean) {
    const state = get(sessionId);
    if (state.active !== id) return;
    set(sessionId, {
      active: null,
      failed: delivered ? state.failed : [...state.failed, id],
      messages: delivered ? state.messages.filter((message) => message.id !== id) : state.messages,
    });
  },
};
