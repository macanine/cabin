// Pure logic for project sync in the UI: the modes as people read them,
// default rules, warnings and summaries. The rules themselves are applied
// by the host app and cabind (core/src/sync).
import type { GitHandling, SyncMode, SyncPreview, SyncRules, SyncStatus } from "../services/ipc/generated/core";
import { formatBytes, formatInteger } from "./format.ts";

export interface ModeInfo {
  mode: SyncMode;
  label: string;
  /** "Local → machine" or "Local ⇄ machine". */
  flow: string;
  summary: string;
  /** What happens to an agent's edits. */
  agentEdits: string;
}

export const SYNC_MODES: ModeInfo[] = [
  {
    mode: "twoWay",
    label: "Two-way",
    flow: "Local ⇄ machine",
    summary: "Changes flow both ways. Agents' edits appear locally, and yours appear in the machine.",
    agentEdits: "Copied back to your local folder. A file changed on both sides is a conflict for you to settle.",
  },
  {
    mode: "safe",
    label: "Local → machine",
    flow: "Local → machine",
    summary: "Local changes flow into the machine. Files agents change or add there are kept, never overwritten.",
    agentEdits: "Kept in the machine. A file changed on both sides is shown as a conflict for you to settle.",
  },
  {
    mode: "mirror",
    label: "Mirror",
    flow: "Local → machine",
    summary: "The machine mirrors your local folder. Simplest; good for reading and running.",
    agentEdits: "Replaced by your local version on the next change. Use for projects agents only read.",
  },
];

/** New projects sync both ways unless told otherwise. */
export const DEFAULT_SYNC_MODE: SyncMode = "twoWay";

export const modeInfo = (mode: SyncMode) => SYNC_MODES.find((m) => m.mode === mode) ?? SYNC_MODES[0];

export const GIT_CHOICES: { value: GitHandling; label: string; detail: string }[] = [
  { value: "copyOnce", label: "Copy once", detail: "The machine gets your history now; then each side commits on its own. Never corrupts either repository." },
  { value: "sync", label: "Keep in sync", detail: "Every change to .git is synced. Fine for an exact mirror; risky two-way if git runs on both sides." },
  { value: "skip", label: "Don't copy", detail: "The machine's copy is not a git repository." },
];

/** The `.git` handling that suits a mode: one writer may sync it. */
export function defaultGit(mode: SyncMode): GitHandling {
  return mode === "mirror" ? "sync" : "copyOnce";
}

export function defaultRules(mode: SyncMode = DEFAULT_SYNC_MODE): SyncRules {
  return { gitignore: true, ignore: [], excluded: [], git: defaultGit(mode) };
}

/** A warning worth showing for this combination, or null. */
export function gitWarning(mode: SyncMode, git: GitHandling): string | null {
  if (git !== "sync") return null;
  if (mode === "twoWay") {
    return "Two-way syncing .git can break the repository if git runs locally and in the machine at the same time. “Copy once” is safer.";
  }
  if (mode === "safe") {
    return "With agent edits kept, .git can end up mixing both sides' state. “Copy once” is safer.";
  }
  return null;
}

/** "my App!" → "my-app" */
export function slug(name: string): string {
  return name.trim().toLowerCase().replace(/[^a-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "");
}

/** Where a local folder lands in the machine by default. */
export function guestPathFor(name: string): string {
  const base = slug(name) || "project";
  return `~/projects/${base}`;
}

/** The folder's own name from a path. */
export function folderName(path: string): string {
  const separator = /^(?:[a-z]:|\\\\)/i.test(path) ? /[\\/]/ : /\//;
  return path.split(separator).filter(Boolean).pop() ?? "";
}

/** Tick or untick a top-level item. */
export function toggleExcluded(rules: SyncRules, name: string, include: boolean): SyncRules {
  const excluded = rules.excluded.filter((path) => path !== name);
  return { ...rules, excluded: include ? excluded : [...excluded, name].sort() };
}

/** Patterns typed one per line → the rules' list (blank and comment lines dropped). */
export function parsePatterns(text: string): string[] {
  return text.split("\n").map((line) => line.trim()).filter((line) => line && !line.startsWith("#"));
}

export function previewSummary(preview: SyncPreview): string {
  return `${formatInteger(preview.files)} ${preview.files === 1 ? "file" : "files"} · ${formatBytes(preview.bytes)}`;
}

export const REASON_LABEL: Record<string, string> = {
  ignored: "ignored",
  unticked: "left out",
  git: "git history",
};

/** One line for a sync's state, as the sidebar and panel show it. */
export function syncLine(status: SyncStatus, now: number = Date.now()): { tone: "ok" | "busy" | "warn" | "bad" | "off"; text: string } {
  if (status.conflicts.length) {
    return { tone: "warn", text: `${status.conflicts.length} ${status.conflicts.length === 1 ? "conflict" : "conflicts"} to settle` };
  }
  switch (status.state) {
    case "offline": return { tone: "off", text: "Waits for the machine" };
    case "paused": return { tone: "off", text: "Paused" };
    case "scanning": return { tone: "busy", text: "Comparing…" };
    case "syncing": return { tone: "busy", text: "Syncing…" };
    case "error": return { tone: "bad", text: status.error ?? "Sync stopped" };
    case "watching": {
      if (status.error) return { tone: "warn", text: status.error };
      const ago = status.lastSyncAt == null ? null : Math.max(0, Math.round((now - status.lastSyncAt) / 1000));
      return { tone: "ok", text: ago == null ? "In sync" : ago < 5 ? "In sync · just now" : `In sync · ${ago < 60 ? `${ago}s` : `${Math.round(ago / 60)}m`} ago` };
    }
  }
}
