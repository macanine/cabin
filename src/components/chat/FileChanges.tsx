import { IconFileDiff } from "@tabler/icons-react";
import { useState } from "react";
import type { FileChangeView } from "../../services/ipc/generated/core";
import { Diff } from "../diff/Diff";
import { Stat, StatBar } from "../diff/Stat";
import { DisclosureChevron, Reveal } from "../motion";

export { Diff };

/** Where a change was found: reported by the agent, or found by diffing the workspace. */
const ORIGIN: Record<FileChangeView["origin"], string> = {
  provider: "Reported by the agent",
  workspace: "Found in the workspace",
};
const KIND: Record<FileChangeView["kind"], string> = { added: "Added", modified: "Modified", deleted: "Deleted", unknown: "Changed" };
const LETTER: Record<FileChangeView["kind"], string> = { added: "A", modified: "M", deleted: "D", unknown: "·" };
const BADGE: Record<FileChangeView["kind"], string> = {
  added: "bg-ok/15 text-ok",
  modified: "bg-warn/15 text-warn",
  deleted: "bg-bad/15 text-bad",
  unknown: "bg-hover text-muted",
};

/** "src/lib/a.ts" → ["src/lib/", "a.ts"]: the name stands out, the folder recedes. */
function splitPath(path: string): [string, string] {
  const cut = path.lastIndexOf("/");
  return cut < 0 ? ["", path] : [path.slice(0, cut + 1), path.slice(cut + 1)];
}

const keyOf = (change: FileChangeView) => `${change.origin}:${change.path}`;

function Row({ change, open, onToggle, showOrigin }: { change: FileChangeView; open: boolean; onToggle: () => void; showOrigin: boolean }) {
  const expandable = change.patch != null && change.patch.length > 0;
  const [seen, setSeen] = useState(open);
  const [dir, name] = splitPath(change.path);
  return (
    <li className="border-t border-line/60 first:border-t-0">
      <button
        type="button"
        disabled={!expandable}
        aria-expanded={open}
        title={change.path}
        onClick={() => { setSeen(true); onToggle(); }}
        className="press flex w-full cursor-pointer items-center gap-2 border-0 bg-transparent px-3 py-2 text-left text-[13px] text-fg enabled:hover:bg-hover disabled:cursor-default"
      >
        <DisclosureChevron open={open} size={13} className={expandable ? "text-faint" : "invisible"} />
        <span className={`grid size-[18px] shrink-0 place-items-center rounded-[5px] font-mono text-[10.5px] font-semibold ${BADGE[change.kind]}`} title={KIND[change.kind]}>{LETTER[change.kind]}</span>
        <span className="min-w-0 flex-1 truncate font-mono text-xs"><span className="text-faint">{dir}</span><span className="font-semibold">{name}</span></span>
        <StatBar additions={change.additions} deletions={change.deletions} />
        <Stat additions={change.additions} deletions={change.deletions} />
        {showOrigin && <span className="shrink-0 rounded-full bg-hover px-1.5 py-px text-[10.5px] text-muted" title={ORIGIN[change.origin]}>{change.origin === "provider" ? "agent" : "workspace"}</span>}
      </button>
      {expandable && (
        <Reveal open={open}>
          {seen && <div className="border-t border-line/60 bg-bg/40 text-fg selectable"><Diff patch={change.patch!} path={change.path} kind={change.kind} /></div>}
        </Reveal>
      )}
    </li>
  );
}

/**
 * The one component for file changes, whether the agent reported them or the
 * guest found them by diffing the workspace. It never looks at which agent
 * made them; the origin is shown only when a list mixes both.
 */
export function FileChanges({ changes, title, open = false }: { changes: FileChangeView[]; title?: string; open?: boolean }) {
  const [all, setAll] = useState(false);
  const [opened, setOpened] = useState<ReadonlySet<string>>(() => new Set(open && changes.length <= 3 ? changes.map(keyOf) : []));
  const shown = all ? changes : changes.slice(0, 6);
  const mixed = new Set(changes.map((change) => change.origin)).size > 1;
  const expandable = changes.filter((change) => change.patch);
  const additions = changes.reduce((sum, c) => sum + (c.additions ?? 0), 0);
  const deletions = changes.reduce((sum, c) => sum + (c.deletions ?? 0), 0);
  const everyOpen = expandable.length > 0 && expandable.every((c) => opened.has(keyOf(c)));
  const toggle = (key: string) => setOpened((old) => {
    const next = new Set(old);
    if (next.has(key)) next.delete(key); else next.add(key);
    return next;
  });
  return (
    <div className="glass selectable overflow-hidden rounded-xl">
      {(changes.length > 1 || title) && <div className="flex items-center gap-2 border-b border-line/60 px-3 py-2 text-[13px] font-medium">
        <IconFileDiff size={15} className="shrink-0 text-muted" />
        <span className="min-w-0 flex-1 truncate">{title ?? `${changes.length} ${changes.length === 1 ? "file" : "files"} changed`}</span>
        <StatBar additions={additions} deletions={deletions} />
        <Stat additions={additions} deletions={deletions} />
        {expandable.length > 1 && (
          <button
            type="button"
            className="press cursor-pointer rounded-md border-0 bg-transparent px-1.5 py-0.5 text-xs text-muted hover:bg-hover hover:text-fg"
            onClick={() => {
              if (everyOpen) setOpened(new Set());
              else { setAll(true); setOpened(new Set(expandable.map(keyOf))); }
            }}
          >{everyOpen ? "Collapse all" : "Expand all"}</button>
        )}
      </div>}
      <ul className="m-0 list-none p-0">
        {shown.map((change) => <Row key={keyOf(change)} change={change} open={opened.has(keyOf(change))} onToggle={() => toggle(keyOf(change))} showOrigin={mixed} />)}
      </ul>
      {changes.length > shown.length && (
        <button type="button" className="press w-full cursor-pointer border-0 border-t border-line/60 bg-transparent py-1.5 text-xs text-muted hover:bg-hover hover:text-fg" onClick={() => setAll(true)}>
          Show {changes.length - shown.length} more
        </button>
      )}
    </div>
  );
}
