import { useCallback, useEffect, useSyncExternalStore } from "react";
import type { ChatState } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { libraryIpc } from "../../services/ipc/library";
import type { QueuedMessage } from "../../lib/agentControls";
import { chatQueue } from "../../lib/chatQueue";
import { uiActions } from "../../state/ui";

export function useMessageQueue({ sessionId, state, blocked, onSent }: {
  sessionId: string;
  state: ChatState["state"];
  blocked: boolean;
  onSent: () => void;
}) {
  const { messages, active, failed } = useSyncExternalStore(chatQueue.subscribe, () => chatQueue.get(sessionId));
  const canSend = !blocked && active === null;
  const canRetry = canSend && (state === "ready" || state === "stopped" || state === "error");
  const deliver = useCallback(async (message: QueuedMessage, mode: "auto" | "retry" | "steer") => {
    if (!chatQueue.start(sessionId, message.id, mode !== "auto")) return;
    try {
      if (mode === "steer") await libraryIpc.chatSteer(sessionId, message.text, []);
      else await libraryIpc.chatSend(sessionId, message.text, []);
    } catch (error) {
      chatQueue.finish(sessionId, message.id, false);
      uiActions.toast(toIpcError(error).message, "error");
      return;
    }
    chatQueue.finish(sessionId, message.id, true);
    onSent();
  }, [sessionId, onSent]);

  useEffect(() => {
    if (state === "ready" && canSend && failed.length === 0 && messages[0]) void deliver(messages[0], "auto");
  }, [state, canSend, deliver, failed, messages]);

  const queue = messages.filter(({ id }) => id !== active);
  return {
    queue,
    failed,
    sending: active !== null,
    add: (text: string) => chatQueue.add(sessionId, text),
    remove: ({ id }: QueuedMessage) => chatQueue.remove(sessionId, id),
    move(from: number, to: number) {
      if (queue[from] && queue[to]) chatQueue.move(sessionId, queue[from].id, queue[to].id);
    },
    retry: canRetry ? (message: QueuedMessage) => void deliver(message, "retry") : undefined,
    startDirect: () => chatQueue.startDirect(sessionId),
    steer: (message: QueuedMessage) => {
      if (canSend) void deliver(message, "steer");
    },
  };
}
