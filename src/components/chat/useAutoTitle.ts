import { useEffect, useMemo, useRef } from "react";
import type { ChatItem, Session } from "../../services/ipc/generated/core";
import { libraryIpc } from "../../services/ipc/library";
import { autoSessionName, usableSessionTitle } from "../../lib/sessionTitle";

/**
 * The agent names its own conversation (ACP `session_info_update`). Adopt
 * that title for the header and sidebar, but only while the name is still
 * the automatic one this session started with: a name the user chose is
 * never overwritten.
 */
export function useAutoTitle(sessionId: string, session: Session | null, items: ChatItem[], title: string | null | undefined) {
  const synced = useRef<string | null>(null);
  const syncing = useRef(false);
  const firstPrompt = useMemo(
    () => items.find((item) => item.kind === "user_message")?.text ?? "",
    [items],
  );

  useEffect(() => {
    synced.current = null;
    syncing.current = false;
  }, [sessionId]);

  useEffect(() => {
    const next = usableSessionTitle(title);
    if (!next || !session) return;
    if (next === session.name) {
      synced.current = next;
      return;
    }
    if (syncing.current) return;
    if (session.name !== autoSessionName(firstPrompt) && session.name !== synced.current) return;
    syncing.current = true;
    libraryIpc.updateSession(sessionId, { name: next })
      .then(() => { synced.current = next; })
      .catch(() => { /* The agent's title is a convenience; the session keeps its name. */ })
      .finally(() => { syncing.current = false; });
  }, [title, session, firstPrompt, sessionId]);
}
