/**
 * Terminal sizing. The emulator follows its box on every frame (so the
 * canvas never overflows or leaves gaps while a window is dragged), but the
 * PTY — and with it the program's reflow and redraw — only gets the size once
 * it has settled. Agent TUIs redraw their whole screen on each SIGWINCH;
 * sending every intermediate size made them stutter and leave artifacts.
 */

export interface Grid {
  cols: number;
  rows: number;
}

/** Below this the program could not draw anything useful. */
export const MIN_GRID: Grid = { cols: 20, rows: 5 };

/** Delay after the last size change before the PTY is resized. */
export const PTY_SETTLE_MS = 90;

/**
 * Cells that fit in a `width` × `height` box (CSS px) for a cell size.
 * Null while the box or the font is not measurable yet (hidden, loading).
 */
export function gridFor(width: number, height: number, cell: { width: number; height: number }): Grid | null {
  if (!(width > 0 && height > 0 && cell.width > 0 && cell.height > 0)) return null;
  return {
    cols: Math.max(MIN_GRID.cols, Math.floor(width / cell.width)),
    rows: Math.max(MIN_GRID.rows, Math.floor(height / cell.height)),
  };
}

/**
 * Calls `send` with the latest grid once no new one arrived for `delay` ms;
 * a grid equal to the last one sent is not sent again. `flush` sends now.
 */
export function settledSender(send: (grid: Grid) => void, delay = PTY_SETTLE_MS) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let latest: Grid | null = null;
  let sent = "";
  const deliver = () => {
    timer = undefined;
    if (!latest) return;
    const key = `${latest.cols}x${latest.rows}`;
    if (key === sent) return;
    sent = key;
    send(latest);
  };
  return {
    push(grid: Grid) {
      latest = grid;
      clearTimeout(timer);
      timer = setTimeout(deliver, delay);
    },
    flush() {
      clearTimeout(timer);
      deliver();
    },
    cancel() {
      clearTimeout(timer);
      timer = undefined;
    },
  };
}
