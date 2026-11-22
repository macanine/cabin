import { useCallback, useEffect, useRef, useState } from "react";
import type { GitStatus } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { workbenchIpc, type WorkbenchRoot } from "../../services/ipc/workbench";

export interface GitStatusState {
  status: GitStatus | null;
  error: string | null;
  loading: boolean;
  refresh: () => Promise<void>;
  /** Take a status a mutation returned, without another round trip. */
  accept: (next: GitStatus) => void;
}

/**
 * The session root's `git status`, polled while the window is visible: every
 * few seconds with the panel open, slowly otherwise (the Changes badge).
 * The guest runs status without optional locks, so polling never blocks an
 * agent's own git commands.
 */
export function useGitStatus(root: WorkbenchRoot, panelOpen: boolean): GitStatusState {
  const [status, setStatus] = useState<GitStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const inflight = useRef(false);
  const key = `${root.workspaceId}:${root.sessionId ?? ""}`;
  const rootRef = useRef(root);
  rootRef.current = root;

  const refresh = useCallback(async () => {
    if (inflight.current) return;
    inflight.current = true;
    setLoading(true);
    try {
      setStatus(await workbenchIpc.gitStatus(rootRef.current));
      setError(null);
    } catch (err) {
      setError(toIpcError(err).message);
    } finally {
      inflight.current = false;
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    setStatus(null);
    setError(null);
  }, [key]);

  useEffect(() => {
    void refresh();
    const every = panelOpen ? 4000 : 20000;
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") void refresh();
    }, every);
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => {
      clearInterval(timer);
      window.removeEventListener("focus", onFocus);
    };
  }, [key, panelOpen, refresh]);

  return { status, error, loading, refresh, accept: setStatus };
}
