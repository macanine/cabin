import { DiffModeEnum, DiffView } from "@git-diff-view/react";
import { IconCheck, IconCopy, IconLayoutColumns, IconLayoutRows, IconTextWrap } from "@tabler/icons-react";
import { useMemo, useState } from "react";
import { clipboard } from "../../services/ipc/dialogs";
import { toHunk } from "../../lib/patch";
import { useScheme } from "../../lib/useScheme";

const MODE_KEY = "cabin.diffMode";
const WRAP_KEY = "cabin.diffWrap";
/** Past this size highlighting is skipped: the diff stays readable and fast. */
const HIGHLIGHT_LIMIT = 250_000;

function stored(key: string, fallback: string): string {
  try { return localStorage.getItem(key) ?? fallback; } catch { return fallback; }
}
function remember(key: string, value: string) {
  try { localStorage.setItem(key, value); } catch { /* per viewer only */ }
}

const tool = "press grid size-6 cursor-pointer place-items-center rounded-md border-0 bg-transparent p-0 text-muted hover:bg-hover hover:text-fg";

/**
 * A unified diff drawn by @git-diff-view/react: line numbers, word-level
 * highlights, syntax colours, inline or side-by-side. `format` makes the mode
 * the caller's; otherwise a small toolbar lets the reader pick and remembers it.
 */
export function Diff({ patch, path = "file", kind, format, toolbar = true }: {
  patch: string;
  path?: string;
  kind?: "added" | "deleted" | "modified" | "unknown";
  format?: "line-by-line" | "side-by-side";
  toolbar?: boolean;
}) {
  const scheme = useScheme();
  const [mode, setMode] = useState<"unified" | "split">(() => (stored(MODE_KEY, "unified") === "split" ? "split" : "unified"));
  const [wrap, setWrap] = useState(() => stored(WRAP_KEY, "0") === "1");
  const [copied, setCopied] = useState(false);
  const hunk = useMemo(() => toHunk(patch, path, kind), [patch, path, kind]);
  const effective = format ? (format === "side-by-side" ? "split" : "unified") : mode;
  if (!hunk) return <div className="px-3 py-2 text-sm text-faint">No textual changes.</div>;

  return (
    <div className="diff-card group/diff relative" aria-label="Diff">
      {toolbar && (
        <div className="absolute top-1.5 right-2 z-10 flex gap-0.5 rounded-lg bg-surface/80 p-0.5 opacity-0 shadow-sm backdrop-blur transition-opacity duration-200 group-hover/diff:opacity-100 focus-within:opacity-100">
          {!format && (
            <button type="button" className={tool} title={mode === "split" ? "Inline diff" : "Side-by-side diff"} aria-label="Toggle side-by-side" onClick={() => { const next = mode === "split" ? "unified" : "split"; setMode(next); remember(MODE_KEY, next); }}>
              {mode === "split" ? <IconLayoutRows size={14} /> : <IconLayoutColumns size={14} />}
            </button>
          )}
          <button type="button" className={`${tool} ${wrap ? "!bg-accent-soft !text-accent" : ""}`} title="Wrap long lines" aria-label="Wrap long lines" aria-pressed={wrap} onClick={() => { setWrap(!wrap); remember(WRAP_KEY, wrap ? "0" : "1"); }}>
            <IconTextWrap size={14} />
          </button>
          <button type="button" className={tool} title="Copy patch" aria-label="Copy patch" onClick={() => { void clipboard.write(patch); setCopied(true); setTimeout(() => setCopied(false), 1400); }}>
            {copied ? <IconCheck size={14} className="animate-fade text-ok" /> : <IconCopy size={14} />}
          </button>
        </div>
      )}
      <DiffView
        data={{ oldFile: { fileName: path }, newFile: { fileName: path }, hunks: [hunk] }}
        diffViewMode={effective === "split" ? DiffModeEnum.Split : DiffModeEnum.Unified}
        diffViewTheme={scheme}
        diffViewHighlight={patch.length < HIGHLIGHT_LIMIT}
        diffViewWrap={wrap}
        diffViewFontSize={12}
      />
    </div>
  );
}
