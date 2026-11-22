/** Ask the session's chat composer to insert text (an `@file` mention). */
export const MENTION_EVENT = "cabin:mention";
export interface MentionDetail { sessionId: string; text: string }

export function mention(sessionId: string, text: string) {
  window.dispatchEvent(new CustomEvent<MentionDetail>(MENTION_EVENT, { detail: { sessionId, text } }));
}
