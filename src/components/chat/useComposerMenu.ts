import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { useEffect, useMemo, useState, type KeyboardEvent } from "react";
import type { AgentCommand, AgentKind, ChatControls } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { libraryIpc } from "../../services/ipc/library";
import { applyItem, menuItems, menuTrigger, type MenuTrigger } from "../../lib/composerMenu";
import { liveCommands, mergeCommands } from "../../lib/agentControls";
import { loadCatalog } from "../../state/agentCatalog";

interface Pool { items: AgentCommand[]; loading: boolean; error: string | null; loaded: boolean }
const EMPTY: Pool = { items: [], loading: false, error: null, loaded: false };

/**
 * The composer's `/` (commands, skills) and `@` (workspace files) menus: what
 * triggers them, their lazily loaded catalogs, the highlighted entry and the
 * keys that drive them. A dismissal (Esc) lasts until the text changes.
 */
export function useComposerMenu({ text, caret, workspaceId, agent, sessionId, worktree, controls, onApply }: {
  text: string;
  caret: number;
  workspaceId: string;
  agent: AgentKind;
  sessionId: string;
  /** Changing worktree changes what `@` and skills see. */
  worktree: string | null;
  controls: ChatControls;
  onApply: (text: string, caret: number) => void;
}) {
  const [catalog, setCatalog] = useState<Pool>(EMPTY);
  const [files, setFiles] = useState<Pool>(EMPTY);
  const [index, setIndex] = useState(0);
  const [dismissed, setDismissed] = useState<string | null>(null);

  const trigger: MenuTrigger | null = useMemo(() => {
    const found = menuTrigger(text, caret);
    return found && dismissed !== `${found.start}:${text}` ? found : null;
  }, [text, caret, dismissed]);
  const commands = useMemo(() => mergeCommands(catalog.items, liveCommands(controls)), [catalog.items, controls]);
  const pool = useMemo(() => [...commands, ...files.items], [commands, files.items]);
  const items = useMemo(() => (trigger ? menuItems(pool, trigger) : []), [trigger, pool]);
  const source = trigger?.sigil === "@" ? files : catalog;
  const loading = !source.loaded;
  const open = trigger != null && (items.length > 0 || loading || !trigger.query);

  useEffect(() => setIndex(0), [trigger?.sigil, trigger?.query]);
  useEffect(() => { setCatalog(EMPTY); setFiles(EMPTY); }, [workspaceId, agent, sessionId, worktree]);

  function fetchCatalog() {
    if (catalog.loading) return;
    setCatalog((old) => ({ ...old, loading: true, error: null }));
    return loadCatalog(workspaceId, agent, false, sessionId).then(
      (next) => setCatalog({ items: next.commands, loading: false, error: next.error, loaded: true }),
      (error) => setCatalog((old) => ({ ...old, loading: false, error: toIpcError(error).message, loaded: true })),
    );
  }

  function fetchFiles() {
    if (files.loading) return;
    setFiles((old) => ({ ...old, loading: true, error: null }));
    return libraryIpc.workspaceFiles(workspaceId, sessionId).then(
      (list) => setFiles({
        items: list.map((path) => ({ name: path, description: null, hint: null, kind: "file", source: "workspace", insert: `@${path} ` })),
        loading: false, error: null, loaded: true,
      }),
      (error) => setFiles((old) => ({ ...old, loading: false, error: toIpcError(error).message, loaded: true })),
    );
  }

  useAutoRefresh(fetchFiles, { enabled: trigger?.sigil === "@", interval: 15_000 });
  useAutoRefresh(fetchCatalog, { enabled: trigger != null && trigger.sigil !== "@", interval: 30_000 });

  function pick(item: AgentCommand) {
    if (!trigger) return;
    const next = applyItem(text, trigger, item);
    onApply(next.text, next.caret);
  }

  /** Handles the key if the menu is open; returns whether it did. */
  function onKey(event: KeyboardEvent): boolean {
    if (!open || !trigger) return false;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : -1;
      setIndex((current) => (items.length ? (current + step + items.length) % items.length : 0));
      return true;
    }
    if ((event.key === "Enter" || event.key === "Tab") && !event.shiftKey && items[index]) {
      event.preventDefault();
      pick(items[index]);
      return true;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      setDismissed(`${trigger.start}:${text}`);
      return true;
    }
    return false;
  }

  return {
    open, trigger, items, index, setIndex, loading, error: source.error, pick, onKey,
  };
}
