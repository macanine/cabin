// Ranking for the command palette. No React here so `node --test` can exercise it.

export interface PaletteItem {
  id: string;
  label: string;
  /** Quiet text on the right: a project, a shortcut. */
  hint?: string;
  group: "Tasks" | "New" | "Go to";
  /** Extra words that match but are not shown. */
  keywords?: string;
  /** Only offered once something is typed (archived tasks, rarer commands). */
  onSearchOnly?: boolean;
}

export const PALETTE_LIMIT = 40;

/** Word starts beat matches inside words; the label beats the hint and keywords. 0 is no match. */
export function scoreItem(item: PaletteItem, query: string): number {
  const words = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return item.onSearchOnly ? 0 : 1;
  const label = item.label.toLocaleLowerCase();
  const rest = `${item.hint ?? ""} ${item.keywords ?? ""}`.toLocaleLowerCase();
  let total = 0;
  for (const word of words) {
    if (label.startsWith(word)) total += 8;
    else if (startsWord(label, word)) total += 6;
    else if (label.includes(word)) total += 3;
    else if (startsWord(rest, word)) total += 2;
    else if (rest.includes(word)) total += 1;
    else return 0;
  }
  return total;
}

function startsWord(text: string, word: string): boolean {
  for (let at = text.indexOf(word); at >= 0; at = text.indexOf(word, at + 1)) {
    if (at === 0 || /[\s/_.:-]/.test(text[at - 1])) return true;
  }
  return false;
}

/** Matches grouped in first-seen group order; equal scores keep the given order. */
export function rankPalette<T extends PaletteItem>(items: T[], query: string, limit = PALETTE_LIMIT): { group: string; items: T[] }[] {
  const scored = items
    .map((item, index) => ({ item, index, score: scoreItem(item, query) }))
    .filter((entry) => entry.score > 0);
  if (query.trim()) scored.sort((a, b) => b.score - a.score || a.index - b.index);
  const groups = new Map<string, T[]>();
  for (const { item } of scored.slice(0, limit)) {
    const list = groups.get(item.group);
    if (list) list.push(item);
    else groups.set(item.group, [item]);
  }
  return [...groups].map(([group, list]) => ({ group, items: list }));
}
