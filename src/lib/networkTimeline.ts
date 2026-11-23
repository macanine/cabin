import type { NetworkHistoryEntry } from "../services/ipc/generated/core";

const FIELDS = [
  ["transport", "Observed address", "Exit IP"],
  ["timezone", "Machine", "VM time zone"],
  ["timezone", "Exit", "Exit time zone"],
  ["language", "Machine", "VM language"],
  ["identity", "Hostname", "VM hostname"],
  ["identity", "Login account", "VM account"],
  ["browser", "Installed time zone", "Chrome time zone"],
  ["browser", "Installed languages", "Chrome languages"],
  ["browser", "Installed language", "Chrome language"],
  ["browser", "Installed platform", "Chrome platform"],
  ["browserLive", "Live time zone", "Live Chrome time zone"],
  ["browserLive", "Live languages", "Live Chrome languages"],
  ["browserLive", "Live language", "Live Chrome language"],
  ["browserLive", "Live platform", "Live Chrome platform"],
] as const;

export function networkTimeline(entries: readonly NetworkHistoryEntry[]) {
  const previous = new Map<string, string>();
  return entries.slice().sort((a, b) => a.id - b.id).flatMap(({ id, at, report }) => {
    if (!report) return [];
    const changes = FIELDS.flatMap(([checkId, evidenceLabel, label]) => {
      const check = report.checks.find((item) => item.id === checkId);
      const after = check?.evidence.find((item) => item.label === evidenceLabel)?.value.trim();
      if (!after || after === "unknown") return [];
      const before = previous.get(label);
      previous.set(label, after);
      return before === undefined || before === after ? [] : [{ label, before, after }];
    });
    return changes.length ? [{ id, at, changes }] : [];
  }).reverse();
}
