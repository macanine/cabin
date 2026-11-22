/** The name the guest derives from a chat's first prompt: the first non-empty
 * line, control characters dropped, at most 60 characters and an ellipsis.
 * Mirrors `App::title_from_prompt` so the UI can tell an untouched automatic
 * name from one the user chose. */
export function autoSessionName(prompt: string): string | null {
  const line = prompt.split(/\r?\n/).map((entry) => entry.trim()).find(Boolean);
  if (!line) return null;
  const chars = [...line].filter((char) => !/\p{Cc}/u.test(char));
  let title = chars.slice(0, 60).join("");
  if ([...line].length > 60) title = `${title.trimEnd()}…`;
  return title || null;
}

/** An agent's own session title, cleaned to what a session name may hold, or
 * null when the agent sent something that cannot be a name. */
export function usableSessionTitle(title: string | null | undefined): string | null {
  const cleaned = (title ?? "").replace(/\s+/g, " ").trim();
  if (!cleaned || [...cleaned].length > 80 || /\p{Cc}/u.test(cleaned)) return null;
  return cleaned;
}
