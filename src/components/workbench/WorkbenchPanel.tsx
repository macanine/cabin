import { IconBrowser, IconFolders, IconGitPullRequest, IconLayoutSidebarRightCollapse } from "@tabler/icons-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { ActionIcon, Tooltip } from "@mantine/core";
import type { WorkbenchRoot } from "../../services/ipc/workbench";
import { changeCount, gitDecorations } from "../../lib/workbench";
import { uiActions, useUi, type PanelTab } from "../../state/ui";
import { FileExplorer } from "./FileExplorer";
import { GitPanel } from "./GitPanel";
import { PreviewBrowser } from "./PreviewBrowser";
import type { GitStatusState } from "./useGitStatus";
import { useI18n } from "../../i18n";

const TABS = [
  { id: "files" as PanelTab, labelKey: "wb.files", icon: IconFolders },
  { id: "changes" as PanelTab, labelKey: "wb.changes", icon: IconGitPullRequest },
  { id: "browser" as PanelTab, labelKey: "wb.browser", icon: IconBrowser },
] as const;

/** Title-bar buttons that open the panel on a tab (the active one closes it). */
export function PanelToggles({ git }: { git: GitStatusState }) {
  const { t } = useI18n();
  const panel = useUi((ui) => ui.panel);
  const count = changeCount(git.status);
  return (
    <div className="wb-toggles" role="tablist" aria-label={t("wb.sidePanel")}>
      {TABS.map(({ id, labelKey, icon: Icon }) => {
        const label = t(labelKey);
        const active = panel.open && panel.tab === id;
        return (
          <Tooltip key={id} label={label}>
            <button role="tab" aria-selected={active} className={`wb-toggle${active ? " active" : ""}`} onClick={() => uiActions.togglePanel(id)} aria-label={label}>
              <Icon size={16} />
              {id === "changes" && count > 0 && <span className="wb-badge">{count > 99 ? "99+" : count}</span>}
            </button>
          </Tooltip>
        );
      })}
    </div>
  );
}

/**
 * The session's side panel: Files, Changes and Browser over the session's
 * root (its worktree, else the workspace). All three stay mounted while the
 * panel is open, so switching tabs keeps the tree, the diff and the page.
 */
export function WorkbenchPanel({ root, git }: { root: WorkbenchRoot; git: GitStatusState }) {
  const { t } = useI18n();
  const panel = useUi((ui) => ui.panel);
  const decorations = useMemo(() => gitDecorations(git.status), [git.status]);
  const [revision, setRevision] = useState(0);
  const count = changeCount(git.status);
  // Reload open folders when the set of changed files moves.
  const signature = useMemo(() => JSON.stringify([git.status?.untracked.length, git.status?.unstaged.length, git.status?.staged.length]), [git.status]);
  useEffect(() => setRevision((r) => r + 1), [signature]);

  const drag = useRef<{ x: number; width: number } | null>(null);
  const [dragging, setDragging] = useState(false);
  useEffect(() => {
    if (!dragging) return;
    const move = (e: PointerEvent) => { if (drag.current) uiActions.resizePanel(drag.current.width + (drag.current.x - e.clientX)); };
    const up = (e: PointerEvent) => {
      if (drag.current) uiActions.resizePanel(drag.current.width + (drag.current.x - e.clientX), true);
      drag.current = null;
      setDragging(false);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    return () => { window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", up); };
  }, [dragging]);

  if (!panel.open) return null;
  return (
    <aside className={`wb-panel${dragging ? " dragging" : ""}`} style={{ width: panel.width }}>
      <div className="wb-resize" onPointerDown={(e) => { drag.current = { x: e.clientX, width: panel.width }; setDragging(true); }}
        onDoubleClick={() => uiActions.resizePanel(440, true)} role="separator" aria-orientation="vertical" aria-label="Resize panel" />
      <div className="wb-tabs" role="tablist">
        {TABS.map(({ id, labelKey, icon: Icon }) => (
          <button key={id} role="tab" aria-selected={panel.tab === id} className={`wb-tab${panel.tab === id ? " active" : ""}`} onClick={() => uiActions.openPanel(id)}>
            <Icon size={14} /> {t(labelKey)}
            {id === "changes" && count > 0 && <span className="wb-count">{count}</span>}
          </button>
        ))}
        <span className="grow" />
        <Tooltip label={t("wb.hidePanel")}>
          <ActionIcon variant="subtle" color="gray" size="sm" aria-label={t("wb.hidePanel")} onClick={() => uiActions.togglePanel()}>
            <IconLayoutSidebarRightCollapse size={16} />
          </ActionIcon>
        </Tooltip>
      </div>
      <div className="wb-body" hidden={panel.tab !== "files"}>
        <FileExplorer root={root} decorations={decorations} sessionId={root.sessionId} revision={revision} visible={panel.tab === "files"} />
      </div>
      <div className="wb-body" hidden={panel.tab !== "changes"}><GitPanel root={root} git={git} /></div>
      <div className="wb-body" hidden={panel.tab !== "browser"}><PreviewBrowser visible={panel.tab === "browser"} /></div>
    </aside>
  );
}
