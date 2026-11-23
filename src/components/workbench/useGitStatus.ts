import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { GitStatus } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { workbenchIpc, type WorkbenchRoot } from "../../services/ipc/workbench";

export interface GitStatusState {
  status: GitStatus | null;
  error: string | null;
  loading: boolean;
  refresh: (force?: boolean) => Promise<void>;
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
  const inflight = useRef<symbol | null>(null);
  const key = `${root.workspaceId}:${root.sessionId ?? ""}`;
  const rootRef = useRef(root);
  rootRef.current = root;

  const refresh = useCallback(async (force = false) => {
    if (inflight.current && !force) return;
    const request = Symbol();
    inflight.current = request;
    const requestedRoot = rootRef.current;
    const isCurrent = () => inflight.current === request
      && requestedRoot.workspaceId === rootRef.current.workspaceId
      && requestedRoot.sessionId === rootRef.current.sessionId;
    setLoading(true);
    try {
      const next = await workbenchIpc.gitStatus(requestedRoot);
      if (!isCurrent()) return;
      setStatus(next);
      setError(null);
    } catch (err) {
      if (isCurrent()) setError(toIpcError(err).message);
    } finally {
      if (isCurrent()) {
        inflight.current = null;
        setLoading(false);
      }
    }
  }, []);

  useLayoutEffect(() => {
    setStatus(null);
    setError(null);
    setLoading(false);
    return () => { inflight.current = null; };
  }, [key]);

  const accept = useCallback((next: GitStatus) => {
    const current = rootRef.current;
    if (key !== `${current.workspaceId}:${current.sessionId ?? ""}`) return;
    inflight.current = null;
    setStatus(next);
    setError(null);
    setLoading(false);
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

  return { status, error, loading, refresh, accept };
}
