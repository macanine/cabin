import { useEffect, useState } from "react";
import type { ChatState } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { libraryIpc } from "../../services/ipc/library";
import { queueMessage, takeQueued, type QueuedMessage } from "../../lib/agentControls";
import { uiActions } from "../../state/ui";

/**
 * Messages typed while the agent works. They go out one at a time, each once
 * the agent is idle again; steering one into the running turn is an explicit
 * action, never the default.
 */
export function useMessageQueue({ sessionId, state, blocked, sending, setSending, onSent }: {
  sessionId: string;
  state: ChatState["state"];
  /** A pending request or an open plan holds the queue. */
  blocked: boolean;
  sending: boolean;
  setSending: (value: boolean) => void;
  onSent: () => void;
}) {
  const [queue, setQueue] = useState<QueuedMessage[]>([]);

  useEffect(() => setQueue([]), [sessionId]);

  useEffect(() => {
    if (state !== "ready" || blocked || sending || !queue.length) return;
    const [next, rest] = takeQueued(queue);
    if (!next) return;
    setQueue(rest);
    setSending(true);
    libraryIpc.chatSend(sessionId, next.text, [])
      .then(onSent)
      .catch((error) => { setQueue((old) => [next, ...old]); uiActions.toast(toIpcError(error).message, "error"); })
      .finally(() => setSending(false));
  }, [state, blocked, sending, queue, sessionId]); // eslint-disable-line react-hooks/exhaustive-deps

  const remove = (message: QueuedMessage) => setQueue((old) => old.filter((entry) => entry.id !== message.id));

  return {
    queue,
    add: (text: string) => setQueue((old) => queueMessage(old, text)),
    remove,
    move(from: number, to: number) {
      setQueue((old) => {
        const next = [...old];
        const [moved] = next.splice(from, 1);
        if (!moved) return old;
        next.splice(to, 0, moved);
        return next;
      });
    },
    async steer(message: QueuedMessage) {
      remove(message);
      try {
        await libraryIpc.chatSteer(sessionId, message.text, []);
        onSent();
      } catch (error) {
        setQueue((old) => [message, ...old]);
        uiActions.toast(toIpcError(error).message, "error");
      }
    },
  };
}
