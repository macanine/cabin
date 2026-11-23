import { queueMessage, type QueuedMessage } from "./agentControls.ts";

interface QueueState {
  messages: QueuedMessage[];
  active: string | symbol | null;
  failed: string[];
}

const empty: QueueState = { messages: [], active: null, failed: [] };
const sessions = new Map<string, QueueState>();
const listeners = new Set<() => void>();

function get(sessionId: string): QueueState {
  return sessions.get(sessionId) ?? empty;
}

function set(sessionId: string, state: QueueState) {
  if (state.messages.length || state.active !== null) sessions.set(sessionId, state);
  else sessions.delete(sessionId);
  listeners.forEach((listener) => listener());
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
