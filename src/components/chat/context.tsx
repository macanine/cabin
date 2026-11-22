import { createContext, useContext, useEffect, useRef, useState } from "react";
import type { ChatAsyncTask, ChatUsage, SubagentTask } from "../../services/ipc/generated/core";

export interface ChatContextValue {
  sessionId: string;
  /** Usage per turn (keyed by the user message that started it). */
  usage: ReadonlyMap<string, ChatUsage>;
  /** Tool calls per turn. */
  tools: ReadonlyMap<string, number>;
  /** Persisted subagents, by ACP task id. */
  subagents: ReadonlyMap<string, SubagentTask>;
  /** Background tasks, by the transcript item (tool card) that started them. */
  tasks: ReadonlyMap<string, ChatAsyncTask>;
  /** Open a subagent's persisted transcript. */
  openSubagent: (taskId: string) => void;
  /** Put an earlier prompt back into the composer. */
  edit: (turn: string) => void;
  /** A child conversation: read and copy, but not steer or restore. */
  readOnly?: boolean;
  /** Ticks every second while the agent works (live timers). */
  now: number;
}

export const ChatContext = createContext<ChatContextValue>({
  sessionId: "",
  usage: new Map(),
  tools: new Map(),
  subagents: new Map(),
  tasks: new Map(),
  openSubagent: () => undefined,
  edit: () => undefined,
  now: 0,
});

export const useChat = () => useContext(ChatContext);

const reducedMotion = () =>
  typeof window !== "undefined" && window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;

/** A number that eases to its new value (count-up), formatted by `format`. */
export function AnimatedNumber({ value, format }: { value: number; format: (value: number) => string }) {
  const [shown, setShown] = useState(value);
  const from = useRef(value);
  useEffect(() => {
    if (reducedMotion() || from.current === value) {
      from.current = value;
      setShown(value);
      return;
    }
    const start = performance.now();
    const origin = from.current;
    let frame = 0;
    const step = (time: number) => {
      const t = Math.min(1, (time - start) / 450);
      const eased = 1 - Math.pow(1 - t, 3);
      const next = origin + (value - origin) * eased;
      from.current = next;
      setShown(next);
      if (t < 1) frame = requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frame);
  }, [value]);
  return <span className="tabular">{format(shown)}</span>;
}
