/** Mouse-wheel input for terminal apps that turned on mouse tracking.
 *
 * ghostty-web 0.4.0 never sends mouse reports: in the alternate screen it
 * turns the wheel into ↑/↓ keys, which full-screen agent UIs read as history
 * navigation instead of scrolling. These helpers encode real wheel reports. */

export const WHEEL_UP = 64;
export const WHEEL_DOWN = 65;

/** Encode one wheel notch at a 1-based cell. SGR (mode 1006) when the app
 * asked for it, otherwise the legacy X10 form (coordinates capped at 223). */
export function wheelReport(button: number, col: number, row: number, sgr: boolean): string {
  if (sgr) return `\x1b[<${button};${col};${row}M`;
  const byte = (value: number) => String.fromCharCode(32 + Math.min(Math.max(value, 1), 223));
  return `\x1b[M${String.fromCharCode(32 + button)}${byte(col)}${byte(row)}`;
}

/** Turns wheel deltas into whole notches, carrying the remainder so slow
 * trackpad scrolls still add up. */
export class WheelAccumulator {
  private carry = 0;

  /** `deltaMode` follows WheelEvent: 0 pixels, 1 lines, 2 pages. Returns
   * signed notches: negative scrolls up. Capped so one fling cannot flood
   * the PTY. */
  take(deltaY: number, deltaMode: number, cellHeight: number, rows: number): number {
    const lines = deltaMode === 1 ? deltaY : deltaMode === 2 ? deltaY * rows : deltaY / Math.max(cellHeight, 1);
    // Reverse direction: drop the leftover from the other way.
    if (Math.sign(lines) !== Math.sign(this.carry)) this.carry = 0;
    this.carry += lines;
    const whole = Math.trunc(this.carry);
    this.carry -= whole;
    return Math.max(-10, Math.min(10, whole));
  }
}

/** 1-based cell under the pointer, clamped to the grid. */
export function cellAt(
  x: number,
  y: number,
  rect: { left: number; top: number; width: number; height: number },
  cols: number,
  rows: number,
): { col: number; row: number } {
  const col = Math.floor(((x - rect.left) / Math.max(rect.width, 1)) * cols) + 1;
  const row = Math.floor(((y - rect.top) / Math.max(rect.height, 1)) * rows) + 1;
  return { col: Math.min(Math.max(col, 1), cols), row: Math.min(Math.max(row, 1), rows) };
}
