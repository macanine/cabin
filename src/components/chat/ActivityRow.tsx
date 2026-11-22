import { IconCheck, IconChevronRight, IconCopy } from "@tabler/icons-react";
import { Fragment, useEffect, useRef, useState, type ReactNode } from "react";
import type { ChatItem } from "../../services/ipc/generated/core";
import { clipboard } from "../../services/ipc/dialogs";
import { Reveal } from "../motion";

/** Monospace text inside a detail box: wrapped, quiet, selectable. */
export const PRE = "m-0 whitespace-pre-wrap font-mono text-xs leading-relaxed text-muted [overflow-wrap:anywhere]";

type Tone = "done" | "running" | "failed" | "stopped";
const toneOf = (status: ChatItem["status"]): Tone =>
  status === "failed" ? "failed"
    : status === "interrupted" || status === "cancelled" ? "stopped"
      : status === "running" || status === "pending" ? "running"
        : "done";

const TILE: Record<Tone, string> = {
  done: "bg-hover text-muted",
  running: "bg-accent-soft text-accent",
  failed: "bg-bad/12 text-bad",
  stopped: "bg-warn/12 text-warn",
};

/** A tool's icon on the activity rail, or a dot for prose. */
export function RailMark({ status = "completed", dot = false, children }: {
  status?: ChatItem["status"];
  /** Prose / a thought: a small dot instead of a tile. */
  dot?: boolean;
  children?: ReactNode;
}) {
  const tone = toneOf(status);
  if (dot) return <span className={`size-1.5 rounded-full transition-colors duration-[var(--motion-fast)] ${tone === "running" ? "animate-breathe bg-accent" : tone === "failed" ? "bg-bad" : "bg-faint/70"}`} aria-hidden />;
  return (
    <span className={`grid size-5 place-items-center rounded-md transition-colors duration-[var(--motion-fast)] ${TILE[tone]} ${tone === "running" ? "animate-breathe" : ""}`} aria-hidden>
      {children}
    </span>
  );
}

/**
 * One row of a turn's activity rail: a marker, a one-line header and an
 * optional detail panel the header toggles. The rail is a hairline drawn
 * between the markers, so it stays correct as rows fold.
 */
export function ActivityRow({
  status = "completed",
  marker,
  markerKind = "ring",
  label,
  title,
  trailing,
  expandable = false,
  defaultOpen = false,
  onActivate,
  addon,
  children,
  className = "",
  itemId,
}: {
  status?: ChatItem["status"];
  marker: ReactNode;
  markerKind?: "ring" | "dot";
  label: ReactNode;
  title?: string;
  trailing?: ReactNode;
  expandable?: boolean;
  defaultOpen?: boolean;
  /** Activate the row instead of unfolding a detail panel (opens a side panel). */
  onActivate?: () => void;
  /** Always-visible content under the header (e.g. a running command's tail). */
  addon?: ReactNode;
  /** The panel under the header; mounted once opened. */
  children?: ReactNode;
  className?: string;
  itemId?: string;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const [seen, setSeen] = useState(defaultOpen);
  // A group follows its own state: it opens while it runs and folds once the
  // reply moves past it. A manual toggle is replaced by the next state change.
  const [autoOpen, setAutoOpen] = useState(defaultOpen);
  if (autoOpen !== defaultOpen) {
    setAutoOpen(defaultOpen);
    setOpen(defaultOpen);
    if (defaultOpen) setSeen(true);
  }
  const interactive = Boolean(onActivate || expandable);
  const head = (
    <>
      <span className="flex min-w-0 flex-1 items-baseline gap-2">{label}</span>
      {interactive && (
        <IconChevronRight size={13} className={`shrink-0 self-center text-faint opacity-0 transition-[transform,opacity] duration-300 ease-out group-hover/row:opacity-100 ${open && expandable ? "rotate-90 opacity-100" : ""}`} aria-hidden />
      )}
      {trailing && <span className="shrink-0 self-center text-xs tabular-nums">{trailing}</span>}
    </>
  );
  const headClass = "flex min-h-7 w-full min-w-0 items-center gap-2 rounded-lg px-2 py-1 text-left text-[13px] text-fg";
  return (
    <li
      className={`group/row relative min-w-0 pl-8 before:absolute before:top-[26px] before:bottom-[-4px] before:left-[9.5px] before:w-px before:bg-line last:before:hidden ${markerKind === "dot" ? "before:top-4" : ""} ${className}`}
      data-status={status}
      data-item-id={itemId}
    >
      <span className="absolute top-1 left-0 grid size-5 place-items-center">{marker}</span>
      {interactive ? (
        <button
          type="button"
          className={`press -ml-2 cursor-pointer border-0 bg-transparent hover:bg-hover ${headClass}`}
          aria-expanded={onActivate ? undefined : open}
          title={title}
          onClick={() => {
            if (onActivate) onActivate();
            else { setSeen(true); setOpen((value) => !value); }
          }}
        >{head}</button>
      ) : (
        <div className={`-ml-2 ${headClass}`} title={title}>{head}</div>
      )}
      {addon}
      {expandable && !onActivate && (
        <Reveal open={open}>{seen && <div className="pt-1 pb-2">{children}</div>}</Reveal>
      )}
    </li>
  );
}

/** The one panel every row's details go in: a quiet box with a footer that
 *  stays put while the body scrolls. */
export function DetailBox({ footer, actions, tone = "neutral", children, className = "" }: {
  footer?: ReactNode;
  actions?: ReactNode;
  tone?: "neutral" | "danger";
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={`overflow-hidden rounded-xl border bg-surface/60 selectable ${tone === "danger" ? "border-bad/35" : "border-line"} ${className}`}>
      <div className="grid max-h-72 gap-1.5 overflow-auto px-3 py-2.5">{children}</div>
      {(footer || actions) && (
        <div className="flex items-center justify-between gap-3 border-t border-line/60 px-3 py-1 text-xs text-faint">
          <span className="truncate tabular-nums">{footer}</span>
          {actions && <span className="flex shrink-0 gap-1">{actions}</span>}
        </div>
      )}
    </div>
  );
}

/** Copy a command, output or diff from a detail panel's footer. */
export function CopyAction({ text, label }: { text: string; label: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  return (
    <button
      type="button"
      className="press inline-flex cursor-pointer items-center gap-1 rounded-md border-0 bg-transparent px-1.5 py-0.5 text-xs text-muted hover:bg-hover hover:text-fg"
      onClick={() => {
        void clipboard.write(text).then(() => {
          setCopied(true);
          clearTimeout(timer.current);
          timer.current = setTimeout(() => setCopied(false), 1500);
        }).catch(() => {});
      }}
    >
      {copied ? <IconCheck size={12} className="animate-fade text-ok" /> : <IconCopy size={12} />}
      {copied ? "Copied" : label}
    </button>
  );
}

/** A disclosure for a turn's steps and failures. */
export function TurnHeader({ meta, open, onToggle }: { meta: ReactNode[]; open: boolean; onToggle: () => void }) {
  return (
    <button type="button" className="press -ml-1 inline-flex w-fit cursor-pointer items-center gap-1.5 rounded-md border-0 bg-transparent px-1 py-0.5 text-xs text-muted hover:bg-hover hover:text-fg" aria-expanded={open} onClick={onToggle}>
      <IconChevronRight size={12} className={`text-faint transition-transform duration-[var(--motion-fast)] ease-out ${open ? "rotate-90" : ""}`} aria-hidden />
      <span>Activity</span>
      {meta.length > 0 && (
        <span className="text-faint tabular-nums">
          {meta.map((piece, index) => <Fragment key={index}> · {piece}</Fragment>)}
        </span>
      )}
    </button>
  );
}
