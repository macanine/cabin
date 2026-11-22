type ReadLine = (index: number) => string | undefined;

/** Capture the top visible row and a little context to distinguish repeated rows. */
export function captureViewport(length: number, offset: number, line: ReadLine) {
  if (offset < 1 || length === 0) return null;
  const index = length - Math.min(length, Math.floor(offset));
  const rows: string[] = [];
  for (let row = index; row < Math.min(length, index + 3); row++) {
    const text = line(row);
    if (text === undefined) break;
    rows.push(text);
  }
  return rows.length ? { index, rows } : null;
}

/** Ghostty evicts old pages as new output arrives, so length alone cannot
 * tell us where a row went. A surviving row can only move toward the oldest
 * end of the buffer. If it aged out, show the oldest row still available. */
export function restoreViewport(anchor: NonNullable<ReturnType<typeof captureViewport>>, length: number, line: ReadLine) {
  for (let index = Math.min(anchor.index, length - anchor.rows.length); index >= 0; index--) {
    if (anchor.rows.every((text, row) => line(index + row) === text)) return length - index;
  }
  return length;
}
