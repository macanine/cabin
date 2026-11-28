/** Ask the session's chat composer to insert text (an `@file` mention). */
export const MENTION_EVENT = "cabin:mention";
export interface MentionDetail { sessionId: string; text: string }

export function mention(sessionId: string, text: string) {
  window.dispatchEvent(new CustomEvent<MentionDetail>(MENTION_EVENT, { detail: { sessionId, text } }));
}

/** Ask the session's workbench Files panel to open a relative workspace path. */
export const OPEN_FILE_EVENT = "cabin:open-file";
export interface OpenFileDetail { sessionId: string; path: string; line?: number | null }

export function openWorkspaceFile(sessionId: string, path: string, line?: number | null) {
  window.dispatchEvent(new CustomEvent<OpenFileDetail>(OPEN_FILE_EVENT, { detail: { sessionId, path, line } }));
}
