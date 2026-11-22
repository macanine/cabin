// Unified-diff text → what the diff view needs, and its line counts.

/** Drop git's preamble (`diff --git`, `index`, mode lines) and make sure the
 *  patch carries `---` / `+++` file lines before its first hunk. */
export function toHunk(patch: string, path: string, kind?: "added" | "deleted" | "modified" | "unknown"): string {
  const lines = patch.replace(/\r\n/g, "\n").replace(/\n$/, "").split("\n");
  const first = lines.findIndex((line) => line.startsWith("@@"));
  if (first < 0) return "";
  const before = lines.slice(0, first);
  const oldLine = before.find((line) => line.startsWith("--- "));
  const newLine = before.find((line) => line.startsWith("+++ "));
  const header = [
    oldLine ?? `--- ${kind === "added" ? "/dev/null" : `a/${path}`}`,
    newLine ?? `+++ ${kind === "deleted" ? "/dev/null" : `b/${path}`}`,
  ];
  return [...header, ...lines.slice(first)].join("\n");
}

/** Added and removed line counts of a unified patch. */
export function patchStats(patch: string): { additions: number; deletions: number } {
  let additions = 0;
  let deletions = 0;
  let inHunk = false;
  for (const line of patch.split("\n")) {
    if (line.startsWith("@@")) inHunk = true;
    else if (!inHunk) continue;
    else if (line.startsWith("+")) additions++;
    else if (line.startsWith("-")) deletions++;
  }
  return { additions, deletions };
}
