// Version lists for the ACP runtime panel. Order and pre-release handling
// match `core/src/agent_runtime/runtimes.rs`.

export function isPrerelease(version: string): boolean {
  return version.includes("-");
}

function parts(version: string): [number[], string | null] {
  const [core, ...pre] = version.split("-");
  return [core.split("+")[0].split(".").map((part) => Number.parseInt(part, 10) || 0), pre.length ? pre.join("-") : null];
}

/** Semver order, a pre-release below its release. */
export function compareVersions(a: string, b: string): number {
  const [coreA, preA] = parts(a);
  const [coreB, preB] = parts(b);
  for (let index = 0; index < Math.max(coreA.length, coreB.length); index++) {
    const diff = (coreA[index] ?? 0) - (coreB[index] ?? 0);
    if (diff) return diff;
  }
  if (preA == null && preB == null) return 0;
  if (preA == null) return 1;
  if (preB == null) return -1;
  const fieldsA = preA.split(".");
  const fieldsB = preB.split(".");
  for (let index = 0; index < Math.min(fieldsA.length, fieldsB.length); index++) {
    const x = fieldsA[index], y = fieldsB[index];
    const nx = Number(x), ny = Number(y);
    const diff = Number.isFinite(nx) && Number.isFinite(ny) ? nx - ny : x.localeCompare(y);
    if (diff) return diff;
  }
  return fieldsA.length - fieldsB.length;
}

/** The newest stable release, if any. */
export function latestStable(versions: string[]): string | null {
  return versions.filter((version) => !isPrerelease(version)).sort((a, b) => compareVersions(b, a))[0] ?? null;
}

/** Whether a newer stable release than the one in use is published. */
export function updateFor(selected: string, published: string[]): string | null {
  const latest = latestStable(published);
  return latest && compareVersions(latest, selected) > 0 ? latest : null;
}

/** Versions to offer in the picker: newest first, pre-releases only on request,
 * always including the ones on disk and the default. */
export function offeredVersions(published: string[], keep: string[], prereleases: boolean): string[] {
  const set = new Set(published.filter((version) => prereleases || !isPrerelease(version)));
  for (const version of keep) set.add(version);
  return [...set].sort((a, b) => compareVersions(b, a));
}
