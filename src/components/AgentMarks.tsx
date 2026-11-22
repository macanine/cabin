import type { AgentKind } from "../services/ipc/generated/core";

/* One simplified glyph per agent: a 24-unit grid, round strokes, drawn in the
   current colour. Wherever an agent is shown it is gray, never its brand
   colour, so lists of agents read as one calm set. */

const MARKS: Record<AgentKind, React.ReactNode> = {
  // A four-stroke spark.
  claude: <path d="M12 3v18M3 12h18M5.6 5.6l12.8 12.8M18.4 5.6 5.6 18.4" />,
  // A hexagon holding a prompt chevron.
  codex: <><path d="M12 2.8 20 7.4v9.2l-8 4.6-8-4.6V7.4z" /><path d="m9.5 10 2.5 2-2.5 2M13.5 14.5h1.5" /></>,
  // A frame around a solid block.
  opencode: <><rect x="4.5" y="3.5" width="15" height="17" rx="2" /><rect x="9" y="8.5" width="6" height="7" rx="0.6" fill="currentColor" stroke="none" opacity="0.45" /></>,
  // The letter pi.
  pi: <path d="M4.5 7h15M9 7v10.5M15 7v7.5q0 3 2.5 3" />,
  // The flared arch from Google's Antigravity mark.
  antigravity: <path d="M3.5 19.5C8.4 15.2 7.4 6.5 12 6.5s3.6 8.7 8.5 13" />,
};

/** An agent's mark in the current text colour, `size` px square. */
export function AgentMark({ agent, size = 16, className }: { agent: AgentKind; size?: number; className?: string }) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.7}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {MARKS[agent]}
    </svg>
  );
}
