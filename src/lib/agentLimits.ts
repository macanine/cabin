// An agent's plan limits in one or two short lines, the way the agent list
// shows them ("5-hour 94% left · Weekly 41% left"). Values come from the
// provider (Claude Code's /usage, Codex app-server), never from local logs.

export interface LimitWindow {
  label: string;
  usedPercent: number;
  resetsAt: number | null;
}

export interface LimitLine {
  text: string;
  /** Under 10% left: shown stronger, like a warning. */
  low: boolean;
  /** Unix ms of the reset, for a tooltip. */
  resetsAt: number | null;
}

/** "5h" / "five hour" / "Session (5h)" → "5-hour"; "7-day" / "weekly" → "Weekly". */
export function shortLimitLabel(label: string): string {
  const text = label.toLowerCase();
  if (/(^|\b)(5\s*-?\s*h|five[ -]hour|5-hour|session)/.test(text)) return "5-hour";
  if (/(7\s*-?\s*d|seven[ -]day|7-day|week)/.test(text)) {
    const model = label.match(/\b(opus|sonnet|fable|haiku)\b/i);
    return model ? `Weekly ${model[1][0].toUpperCase()}${model[1].slice(1).toLowerCase()}` : "Weekly";
  }
  if (/month/.test(text)) return "Monthly";
  return label.trim();
}

export function limitLines(windows: readonly LimitWindow[]): LimitLine[] {
  return windows.map((window) => {
    const left = Math.max(0, Math.min(100, Math.round(100 - window.usedPercent)));
    return {
      text: left === 0 ? `${shortLimitLabel(window.label)} used up` : `${shortLimitLabel(window.label)} ${left}% left`,
      low: left < 10,
      resetsAt: window.resetsAt,
    };
  });
}
