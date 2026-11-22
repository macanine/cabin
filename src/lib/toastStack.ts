// Where each toast sits (components/Toasts.tsx). Collapsed, the newest is in
// front and older ones tuck behind it, smaller and fainter, like a deck.
// Expanded (hover), they fan out into a readable column.

export interface StackSlot {
  /** Offset from the top, px. */
  y: number;
  scale: number;
  opacity: number;
  /** Older toasts sit behind newer ones. */
  z: number;
}

/** How many toasts show behind the front one when collapsed. */
export const VISIBLE_BEHIND = 2;
const PEEK = 9;
const GAP = 8;

/**
 * Slots for toasts listed newest first. `heights` are their measured
 * heights (0 while unknown). Hidden ones (too far back) fade out in place.
 */
export function stackLayout(heights: readonly number[], expanded: boolean): StackSlot[] {
  let offset = 0;
  return heights.map((height, index) => {
    const slot: StackSlot = expanded
      ? { y: offset, scale: 1, opacity: 1, z: heights.length - index }
      : {
          y: Math.min(index, VISIBLE_BEHIND + 1) * PEEK,
          scale: Math.max(0.8, 1 - index * 0.05),
          opacity: index > VISIBLE_BEHIND ? 0 : 1 - index * 0.18,
          z: heights.length - index,
        };
    offset += height + GAP;
    return slot;
  });
}

/** The height the stack takes, so the page can stay clickable around it. */
export function stackHeight(heights: readonly number[], expanded: boolean): number {
  if (!heights.length) return 0;
  if (expanded) return heights.reduce((sum, height) => sum + height, 0) + GAP * (heights.length - 1);
  return heights[0] + Math.min(heights.length - 1, VISIBLE_BEHIND) * PEEK;
}
