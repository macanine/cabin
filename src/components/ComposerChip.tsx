import type { ReactNode } from "react";

/** The small quiet button every composer choice uses (model, permissions, options). */
export const CHIP =
  "press inline-flex h-7 max-w-60 min-w-0 shrink-0 cursor-pointer items-center gap-1.5 rounded-lg border-0 bg-transparent px-2 text-xs font-medium text-muted hover:bg-hover hover:text-fg aria-expanded:bg-hover aria-expanded:text-fg disabled:cursor-default disabled:opacity-45";

/** A tinted rounded tile holding an icon: the leading mark of a menu row. */
export function Tile({ tone = "neutral", children }: { tone?: "neutral" | "accent" | "warn" | "bad" | "info" | "ok"; children: ReactNode }) {
  const cls = { neutral: "bg-hover text-muted", accent: "bg-accent-soft text-accent", warn: "bg-warn/14 text-warn", bad: "bg-bad/13 text-bad", info: "bg-info/13 text-info", ok: "bg-ok/13 text-ok" }[tone];
  return <span className={`grid size-7 shrink-0 place-items-center rounded-lg transition-colors duration-200 ${cls}`}>{children}</span>;
}
