/** Unsent chat messages, kept per session so switching tasks never loses
 * what you were typing. Remembered in this viewer only (localStorage). */

const KEY = "cabin.chatDrafts";
/** Sessions whose drafts are kept; the least recently edited go first. */
export const DRAFT_LIMIT = 30;

export type Drafts = Record<string, { text: string; at: number }>;

/** Drafts with `sessionId` set to `text` (removed when blank), trimmed to `limit`. */
export function withDraft(drafts: Drafts, sessionId: string, text: string, at: number, limit = DRAFT_LIMIT): Drafts {
  const next = { ...drafts };
  delete next[sessionId];
  if (text.trim()) next[sessionId] = { text, at };
  const ids = Object.keys(next);
  if (ids.length > limit) {
    ids.sort((a, b) => next[a].at - next[b].at);
    for (const id of ids.slice(0, ids.length - limit)) delete next[id];
  }
  return next;
}

function read(): Drafts {
  try {
    const value = JSON.parse(localStorage.getItem(KEY) ?? "{}") as unknown;
    return value && typeof value === "object" ? (value as Drafts) : {};
  } catch {
    return {};
  }
}

export function loadDraft(sessionId: string): string {
  const draft = read()[sessionId];
  return typeof draft?.text === "string" ? draft.text : "";
}

export function saveDraft(sessionId: string, text: string) {
  try {
    localStorage.setItem(KEY, JSON.stringify(withDraft(read(), sessionId, text, Date.now())));
  } catch {
    // Not remembered in this viewer; the composer still works.
  }
}
