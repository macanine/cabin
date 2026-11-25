import { ActionIcon, Tooltip } from "@mantine/core";
import type { ReactNode } from "react";
import {
  IconAlertTriangle, IconCircleCheck, IconCircleDashed, IconCircleMinus, IconCircleX, IconLoader2, IconRotate,
  IconTerminal2,
} from "@tabler/icons-react";
import { AgentMark } from "./AgentMarks";
import type { AgentKind, OperationState, SessionStatus } from "../services/ipc/generated/core";
import { STATE_LABEL } from "../lib/operations";

export const AGENT_NAMES: Record<AgentKind, string> = {
  claude: "Claude Code",
  codex: "Codex",
  opencode: "OpenCode",
  pi: "Pi",
  antigravity: "Antigravity",
};

/** What a session runs: an agent's name, or "Terminal" for a plain shell. */
export function sessionKindName(agent: AgentKind | null): string {
  return agent ? AGENT_NAMES[agent] : "Terminal";
}

/** Visual state shared by checks, steps and phases. */
export type StateTone = "ok" | "running" | "bad" | "warn" | "undone" | "skipped" | "pending";

const TONE_ICON = {
  ok: [IconCircleCheck, "var(--ok)"],
  running: [IconLoader2, "var(--info)"],
  bad: [IconCircleX, "var(--bad)"],
  warn: [IconAlertTriangle, "var(--warn)"],
  undone: [IconRotate, "var(--warn)"],
  skipped: [IconCircleMinus, "var(--faint)"],
  pending: [IconCircleDashed, "var(--faint)"],
} as const;

export function StateIcon({ tone, size = 16 }: { tone: StateTone; size?: number }) {
  const [Icon, color] = TONE_ICON[tone];
  return <Icon size={size} color={color} className={tone === "running" ? "animate-spin" : undefined} style={{ flexShrink: 0 }} />;
}

/** An agent's mark, bare and centred in a square box so it lines up with icons and text. */
export function AgentLogo({ agent, size = 18 }: { agent: AgentKind | null; size?: number }) {
  return (
    <span
      className="inline-grid shrink-0 place-items-center text-muted transition-colors duration-200"
      style={{ width: size, height: size }}
      aria-label={sessionKindName(agent)}
      role="img"
    >
      <AgentIcon agent={agent} size={Math.round(size * 0.8)} />
    </span>
  );
}

/** The agent's mark; a terminal (no agent) gets the terminal glyph. */
export function AgentIcon({ agent, size = 16 }: { agent: AgentKind | null; size?: number }) {
  if (agent) return <AgentMark agent={agent} size={size} />;
  return <IconTerminal2 size={size} stroke={1.7} />;
}

export function sessionTone(status: SessionStatus): "ok" | "warn" | "bad" | "" {
  switch (status.state) {
    case "running":
      return "ok";
    case "starting":
      return "warn";
    case "failed":
      return "bad";
    case "exited":
      return status.code === 0 ? "" : "bad";
    default:
      return "";
  }
}

export function sessionLabel(status: SessionStatus): string {
  switch (status.state) {
    case "running":
      return "Running";
    case "starting":
      return "Starting…";
    case "failed":
      return "Failed to start";
    case "exited":
      return status.code == null ? "Exited" : `Exited (${status.code})`;
    default:
      return "Stopped";
  }
}

export function SessionDot({ status }: { status: SessionStatus }) {
  return <span className={`dot ${sessionTone(status)}`} aria-label={sessionLabel(status)} />;
}

export function SessionPill({ status }: { status: SessionStatus }) {
  return (
    <span className={`pill ${sessionTone(status)}`}>
      <SessionDot status={status} />
      {sessionLabel(status)}
    </span>
  );
}

export function operationTone(state: OperationState): "ok" | "warn" | "bad" | "info" | "" {
  switch (state) {
    case "succeeded":
      return "ok";
    case "running":
    case "pending":
      return "info";
    case "failed":
      return "bad";
    case "rolledBack":
    case "cancelled":
      return "warn";
  }
}

export function OperationPill({ state }: { state: OperationState }) {
  return <span className={`pill ${operationTone(state)}`}>{STATE_LABEL[state]}</span>;
}

export function Section({ title, hint, action, children, quiet = false }: { title: string; hint?: ReactNode; action?: ReactNode; children: ReactNode; quiet?: boolean }) {
  return (
    <section className={`mb-[18px] min-w-0 rounded-[var(--card-radius)] border border-line p-4 ${quiet ? "bg-transparent" : "bg-surface"}`}>
      <div className="mb-3 flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h3 className="m-0 text-[13.5px] font-semibold">{title}</h3>
          {hint && <p className="m-0 mt-0.5 text-xs leading-snug text-muted">{hint}</p>}
        </div>
        {action}
      </div>
      {children}
    </section>
  );
}

/** Rows of settings in one card, under an optional heading. */
export function SettingGroup({ title, children }: { title?: string; children: ReactNode }) {
  return <>
    {title && <h3 className="m-0 mb-2 px-1 text-sm font-medium text-muted">{title}</h3>}
    <div className="set-group">{children}</div>
  </>;
}

/** What it is on the left, its control on the right. */
export function SettingRow({ label, hint, children }: { label: ReactNode; hint?: ReactNode; children?: ReactNode }) {
  return (
    <div className="set-row">
      <div className="set-row-copy"><strong>{label}</strong>{hint && <span>{hint}</span>}</div>
      {children && <div className="set-row-control">{children}</div>}
    </div>
  );
}

/** Round icon action (send, stop, start, continue): one shape everywhere. */
export function CircleButton({ label, onClick, children, primary, loading, disabled, className = "" }: {
  label: string;
  onClick: () => void;
  children: ReactNode;
  primary?: boolean;
  loading?: boolean;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <Tooltip label={label} openDelay={300}>
      {/* The wrapper keeps the tooltip working while the button is disabled. */}
      <span className="circle-button-wrap">
      <ActionIcon
        size={30}
        radius="xl"
        variant={primary ? "filled" : "default"}
        className={`circle-button ${className}`}
        aria-label={label}
        loading={loading}
        disabled={disabled}
        onClick={onClick}
      >
        {children}
      </ActionIcon>
      </span>
    </Tooltip>
  );
}
