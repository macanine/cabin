import type { NodeRelease } from "../services/ipc/generated/core";

export type LineStatus = "activeLts" | "maintenanceLts" | "current" | "endOfLife";

/** One release line (a major version) with its newest build. */
export interface ReleaseLine {
  major: number;
  status: LineStatus;
  /** LTS codename of the line, if it is an LTS line. */
  codename: string | null;
  latest: NodeRelease;
  releases: NodeRelease[];
  installed: boolean;
}

export const NODE_MIRRORS = [
  { value: "https://nodejs.org/dist", label: "nodejs.org", hint: "Official downloads" },
  { value: "https://npmmirror.com/mirrors/node", label: "npmmirror", hint: "Fast in mainland China" },
] as const;

export const LINE_STATUS_LABEL: Record<LineStatus, string> = {
  activeLts: "Active LTS",
  maintenanceLts: "Maintenance LTS",
  current: "Current",
  endOfLife: "End-of-life",
};

/** Group releases (newest first) into lines and classify each line. A line
 * is LTS when its newest build has an LTS codename; the newest LTS line is
 * Active, older supported ones are Maintenance; anything below `minimumMajor`
 * or an odd line older than the Active LTS is end-of-life. */
export function releaseLines(releases: NodeRelease[], minimumMajor: number): ReleaseLine[] {
  const byMajor = new Map<number, NodeRelease[]>();
  for (const release of releases) {
    const list = byMajor.get(release.major) ?? [];
    list.push(release);
    byMajor.set(release.major, list);
  }
  const majors = [...byMajor.keys()].sort((a, b) => b - a);
  const activeLts = majors.find((major) => byMajor.get(major)!.some((r) => r.lts != null));
  return majors.map((major) => {
    const list = byMajor.get(major)!;
    const latest = list[0];
    const codename = list.find((r) => r.lts != null)?.lts ?? null;
    let status: LineStatus;
    if (major < minimumMajor) status = "endOfLife";
    else if (codename != null) status = major === activeLts ? "activeLts" : "maintenanceLts";
    else if (activeLts == null || major > activeLts) status = "current";
    else status = "endOfLife";
    return { major, status, codename, latest, releases: list, installed: list.some((r) => r.installed) };
  });
}

/** The newest build of the Active LTS line, else the newest build. */
export function recommendedRelease(lines: ReleaseLine[]): NodeRelease | null {
  return (lines.find((line) => line.status === "activeLts") ?? lines[0])?.latest ?? null;
}

export function archiveUrl(mirror: string, version: string, platform: string): string {
  return `${mirror.replace(/\/+$/, "")}/${version}/node-${version}-${platform}.tar.gz`;
}

export function majorOf(version: string): number | null {
  const match = /^v?(\d+)\./.exec(version.trim());
  return match ? Number(match[1]) : null;
}
