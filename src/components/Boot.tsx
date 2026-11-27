import { IconAlertTriangle, IconCheck, IconX } from "@tabler/icons-react";
import type { ReactNode } from "react";
import { CabinMark } from "./CabinMark";
import { DisclosureChevron, Reveal } from "./motion";

/* The pieces of the machine start and install screens: a centred column, a
   progress ring, a glass step list and a collapsible details area. */

export type Tone = "running" | "ok" | "bad";
export type StepKind = "pending" | "running" | "done" | "failed" | "skipped";

const TONE_TEXT: Record<Tone, string> = { running: "text-accent", ok: "text-ok", bad: "text-bad" };

/** A page-sized, vertically centred column for the boot screens. */
export function BootPage({ children, wide }: { children: ReactNode; wide?: boolean }) {
  return (
    <div className="flex h-full min-h-0 flex-1 overflow-x-hidden overflow-y-auto px-6 py-8">
      <div className={`m-auto flex w-full min-w-0 flex-col gap-5 ${wide ? "max-w-[680px]" : "max-w-[520px]"}`}>{children}</div>
    </div>
  );
}

const R = 30;
const C = 2 * Math.PI * R;

/** A progress ring, a state glyph and the current phase. */
export function BootHero({ tone, progress, title, subtitle, aside }: {
  tone: Tone;
  progress: number | null;
  title: ReactNode;
  subtitle?: ReactNode;
  aside?: ReactNode;
}) {
  const fraction = progress === null || !Number.isFinite(progress) ? null : Math.max(0, Math.min(1, progress));
  const orbiting = tone === "running" && fraction === null;
  return (
    <div className={`grid min-w-0 animate-enter justify-items-center gap-1.5 text-center ${TONE_TEXT[tone]}`}>
      <div className="relative mb-2 grid size-[84px] place-items-center">
        <svg viewBox="0 0 72 72" className="absolute inset-0" aria-hidden>
          <circle cx="36" cy="36" r={R} fill="none" strokeWidth="3" className="stroke-line" />
          <circle
            cx="36" cy="36" r={R} fill="none" strokeWidth="3" strokeLinecap="round"
            className={`origin-center -rotate-90 [transform-box:view-box] stroke-current transition-[stroke-dashoffset,stroke] duration-[var(--motion-progress)] ease-[var(--ease-out)] ${orbiting ? "animate-orbit" : ""}`}
            strokeDasharray={C}
            strokeDashoffset={C * (1 - (fraction ?? (orbiting ? 0.28 : tone === "ok" ? 1 : 0)))}
          />
        </svg>
        <span key={tone} className="grid animate-settle place-items-center" aria-hidden>
          {tone === "ok" ? <IconCheck size={28} stroke={2.4} /> : tone === "bad" ? <IconAlertTriangle size={26} stroke={2} /> : <CabinMark variant="mono" size={24} className="animate-breathe" />}
        </span>
      </div>
      <h1 role="status" className="m-0 text-xl font-semibold tracking-tight text-fg">{title}</h1>
      {subtitle && <p key={typeof subtitle === "string" ? subtitle : undefined} className="m-0 max-w-full animate-fade text-md text-muted [overflow-wrap:anywhere]">{subtitle}</p>}
      {aside}
    </div>
  );
}

/** Elapsed time under the hero. */
export function Elapsed({ children, title }: { children: ReactNode; title?: string }) {
  return <span title={title} className="rounded-full bg-hover px-2.5 py-0.5 font-mono text-xs text-muted tabular-nums">{children}</span>;
}

/** Stage mark: dashed ring, spinning arc, check, cross. */
function StepMark({ state }: { state: StepKind }) {
  const base = "mt-0.5 grid size-[18px] shrink-0 place-items-center rounded-full";
  if (state === "done") return <span className={`${base} animate-settle bg-ok text-white`} aria-hidden><IconCheck size={12} stroke={3} /></span>;
  if (state === "failed") return <span className={`${base} animate-settle bg-bad text-white`} aria-hidden><IconX size={12} stroke={3} /></span>;
  if (state === "running") return <span className={`${base} animate-spin border-2 border-accent border-t-transparent`} aria-hidden />;
  return <span className={`${base} border-[1.5px] border-dashed border-faint/60`} aria-hidden />;
}

/** One step: mark, title, optional live lines, and a time on the right. */
export function BootStep({ state, title, time, order, children }: {
  state: StepKind;
  order: number;
  title: ReactNode;
  time?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <li className={`flex min-w-0 animate-enter items-start gap-3 rounded-lg px-3 py-2.5 transition-colors duration-[var(--motion-fast)] ${state === "running" ? "bg-accent-soft" : ""}`}
      style={{ animationDelay: `${Math.min(order, 4) * 25}ms` }}>
      <StepMark key={state} state={state} />
      <div className="grid min-w-0 flex-1 gap-0.5">
        <div className={`text-md font-medium transition-colors duration-300 ${state === "pending" ? "text-faint" : state === "done" ? "text-muted" : "text-fg"}`}>{title}</div>
        {children}
      </div>
      <span className="shrink-0 pt-0.5 font-mono text-xs text-faint tabular-nums">{time}</span>
    </li>
  );
}

/** A live one-line status. */
export function Ticker({ children, title }: { children: ReactNode; title?: string }) {
  return <div title={title} className="truncate font-mono text-xs text-muted">{children}</div>;
}

/** "Details and logs": a quiet toggle over an eased reveal. */
export function BootDetails({ open, onToggle, children }: { open: boolean; onToggle: () => void; children: ReactNode }) {
  return (
    <div className="grid min-w-0 gap-1">
      <button
        type="button"
        aria-expanded={open}
        onClick={onToggle}
        className="press -ml-1 inline-flex w-fit cursor-pointer items-center gap-1 rounded-md border-0 bg-transparent px-1.5 py-1 text-xs text-faint hover:bg-hover hover:text-muted"
      >
        <DisclosureChevron open={open} />
        {open ? "Hide details" : "Details and logs"}
      </button>
      <Reveal open={open}><div className="grid min-w-0 gap-3 pt-1">{children}</div></Reveal>
    </div>
  );
}

/** A tinted note: error, warning or fact. */
export function Note({ tone = "info", children, role }: { tone?: "info" | "ok" | "warn" | "bad"; children: ReactNode; role?: string }) {
  const tint = { info: "bg-hover text-muted", ok: "bg-ok/10 text-ok", warn: "bg-warn/10 text-warn", bad: "bg-bad/10 text-bad" }[tone];
  return <div role={role} className={`rounded-lg px-3 py-2 text-sm [overflow-wrap:anywhere] ${tint}`}>{children}</div>;
}
