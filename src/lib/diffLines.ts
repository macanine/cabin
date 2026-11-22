// A unified diff split into rows with old/new line numbers, for display.

export type DiffRowKind = "meta" | "hunk" | "add" | "del" | "same" | "note";

export interface DiffRow {
  kind: DiffRowKind;
  /** Line number in the old file (deletions and context). */
  old: number | null;
  /** Line number in the new file (additions and context). */
  new: number | null;
  text: string;
}

const HUNK = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)$/;

export function diffRows(patch: string): DiffRow[] {
  const rows: DiffRow[] = [];
  let oldLine = 0;
  let newLine = 0;
  for (const line of patch.replace(/\n$/, "").split("\n")) {
    if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("Index: ") || line.startsWith("===")) {
      rows.push({ kind: "meta", old: null, new: null, text: line });
      continue;
    }
    const hunk = HUNK.exec(line);
    if (hunk) {
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[2]);
      rows.push({ kind: "hunk", old: null, new: null, text: line });
      continue;
    }
    if (line.startsWith("\\")) {
      rows.push({ kind: "note", old: null, new: null, text: line });
    } else if (line.startsWith("+")) {
      rows.push({ kind: "add", old: null, new: newLine++, text: line.slice(1) });
    } else if (line.startsWith("-")) {
      rows.push({ kind: "del", old: oldLine++, new: null, text: line.slice(1) });
    } else {
      rows.push({ kind: "same", old: oldLine++, new: newLine++, text: line.startsWith(" ") ? line.slice(1) : line });
    }
  }
  return rows;
}

/** Added and removed line counts of a patch. */
export function diffCounts(patch: string): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const row of diffRows(patch)) {
    if (row.kind === "add") added++;
    else if (row.kind === "del") removed++;
  }
  return { added, removed };
}
