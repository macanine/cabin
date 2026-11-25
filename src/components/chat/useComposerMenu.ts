import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";
import type { AgentCommand, AgentKind, ChatControls } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { libraryIpc } from "../../services/ipc/library";
import { applyItem, menuItems, menuKeyAction, menuTrigger } from "../../lib/composerMenu";
import { liveCommands, mergeCommands } from "../../lib/agentControls";
import { sessionCommandFor, sessionCommands } from "../../lib/sessionCommands";
import { loadCatalog } from "../../state/agentCatalog";

interface Pool { scope: string; items: AgentCommand[]; error: string | null }

export function isComposerComposing(event: KeyboardEvent, composing: boolean) {
  return composing || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229;
}

/** Shared completion for the first prompt and live chat. Escape makes the
 * current token literal; changing the draft, token or scope reopens it. */
export function useComposerMenu({ text, caret, workspaceId, agent, sessionId = null, worktree = null,
  newWorktree = false, controls, includeSessionCommands = false, enabled = true, onApply }: {
  text: string;
  caret: number;
  workspaceId: string | null;
  agent: AgentKind;
  sessionId?: string | null;
  worktree?: string | null;
  newWorktree?: boolean;
  controls?: ChatControls;
  includeSessionCommands?: boolean;
  enabled?: boolean;
  onApply: (text: string, caret: number) => void;
}) {
  const inputId = useId();
  const [focused, setFocused] = useState(false);
  const [catalog, setCatalog] = useState<Pool | null>(null);
  const [files, setFiles] = useState<Pool | null>(null);
  const [highlight, setHighlight] = useState<{ key: string; token: string } | null>(null);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const scope = JSON.stringify([workspaceId, agent, sessionId, worktree, newWorktree]);
  const currentScope = useRef(scope);
  currentScope.current = scope;
  const found = useMemo(() => {
    if (!enabled || !workspaceId || (includeSessionCommands && sessionCommandFor(text, false))) return null;
    return menuTrigger(text, caret);
  }, [enabled, workspaceId, includeSessionCommands, text, caret]);
  const key = found ? JSON.stringify([scope, found.start, found.query, text]) : null;
  useEffect(() => { setDismissed((old) => old === key ? old : null); }, [key]);
  const trigger = focused && key !== dismissed ? found : null;
  const open = trigger != null;
  const fileMenu = trigger?.sigil === "@";
  const source = fileMenu ? files : catalog;
  const pool = source?.scope === scope ? source : null;
  const live = useMemo(() => controls ? liveCommands(controls) : [], [controls]);
  const loading = open && !pool;
  const items = useMemo(() => {
    if (!trigger) return [];
    if (fileMenu) return menuItems(pool?.items ?? [], trigger);
    const commands = mergeCommands(pool?.items ?? [], live);
    if (includeSessionCommands && trigger.sigil === "/" && text.trim() === text.slice(trigger.start, trigger.end)) {
      commands.push(...sessionCommands);
    }
    return menuItems(commands, trigger);
  }, [trigger, fileMenu, pool, live, includeSessionCommands, text]);
  const index = highlight && highlight.key === key ? Math.max(0, items.findIndex((item) => item.insert === highlight.token)) : 0;

  useAutoRefresh(async (isCurrent) => {
    if (!workspaceId || newWorktree) return;
    const current = () => isCurrent() && currentScope.current === scope;
    try {
      const list = await libraryIpc.workspaceFiles(workspaceId, sessionId);
      if (current()) setFiles({ scope, error: null, items: list.map((path) => ({ name: path,
        description: null, hint: null, kind: "file", source: null, insert: `@${path} ` })) });
    } catch (error) {
      if (current()) setFiles((old) => ({ scope, items: old?.scope === scope ? old.items : [], error: toIpcError(error).message }));
    }
  }, { enabled: open && fileMenu && !newWorktree, interval: 15_000, refreshKey: scope });

  useAutoRefresh(async (isCurrent) => {
    if (!workspaceId) return;
    const current = () => isCurrent() && currentScope.current === scope;
    try {
      const next = await loadCatalog(workspaceId, agent, false, sessionId, worktree);
      // The catalog's partial error is for MCP; it does not invalidate the skills.
      const items = newWorktree ? next.commands.filter((item) => item.source === "user") : next.commands;
      if (current()) setCatalog({ scope, items, error: null });
    } catch (error) {
      if (current()) setCatalog((old) => ({ scope, items: old?.scope === scope ? old.items : [], error: toIpcError(error).message }));
    }
  }, { enabled: open && !fileMenu, interval: 30_000, refreshKey: scope });

  function setIndex(next: number) {
    if (key && items[next]) setHighlight({ key, token: items[next].insert });
  }

  function pick(item: AgentCommand) {
    if (!trigger) return;
    const next = applyItem(text, trigger, item);
    onApply(next.text, next.caret);
  }

  function onKey(event: KeyboardEvent): boolean {
    if (!open) return false;
    const item = items[index];
    const action = menuKeyAction(event.key, event.shiftKey, Boolean(item), event.repeat);
    if (!action) return false;
    event.preventDefault();
    if (action === "dismiss") setDismissed(key);
    if (action === "pick" && item) pick(item);
    if ((action === "next" || action === "previous") && items.length) {
      setIndex((index + (action === "next" ? 1 : -1) + items.length) % items.length);
    }
    return true;
  }

  return {
    open, trigger, items, index, setIndex, pick, onKey,
    loading: loading && !(fileMenu && newWorktree), error: pool?.error ?? null,
    emptyReason: newWorktree && fileMenu ? "Files are available once the worktree is created." : null,
    skillsOnly: !sessionId,
    inputProps: {
      onFocus: () => setFocused(true),
      onBlur: () => setFocused(false),
      "aria-autocomplete": enabled ? "list" as const : undefined,
      "aria-controls": open ? inputId : undefined,
      "aria-activedescendant": open && items[index] ? `${inputId}-${index}` : undefined,
    },
    listId: inputId,
  };
}
