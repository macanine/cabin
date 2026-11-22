/**
 * Window-chrome geometry. On macOS the window has an overlay title bar: AppKit
 * draws the traffic lights over the web view. The UI keeps a strip across the
 * top of the window (sidebar and content start below it) and a small toolbar
 * right after the lights, centred on the lights' own measured centre — so it
 * lines up whatever height the strip ends up with. The real frames come from
 * `window_chrome` (they differ between macOS releases); the defaults cover the
 * first paint and other platforms.
 */
import type { WindowChrome } from "../services/ipc/generated/common";

/** Strip height when there are no window controls to fit around. */
export const STRIP_DEFAULT = 40;
const STRIP_MIN = 32;
/** Gap between the zoom button and the toolbar. */
const CONTROLS_GAP = 10;
/** Left padding of the toolbar when there are no traffic lights. */
const BARE_INSET = 8;
/** Reserve space for AppKit's controls before the first measurement. */
export const MAC_DEFAULT: ChromeLayout = { strip: 48, inset: 100, center: 24 };
const GEOMETRY_SETTLE_MS = 120;

export interface ChromeLayout {
  /** Height of the window strip above the sidebar and content, px. */
  strip: number;
  /** Left edge of the window toolbar, px. */
  inset: number;
  /** Vertical centre of the window toolbar (= the lights' centre), px. */
  center: number;
}

export function chromeLayout(chrome: WindowChrome | null): ChromeLayout {
  if (chrome) {
    const center = chrome.controlsCenterY;
    const strip = Math.max(STRIP_MIN, Math.ceil(center * 2));
    const inset = Math.ceil(chrome.controlsRight + CONTROLS_GAP);
    if (chrome.controlsRight > 0 && center > 0 && Number.isFinite(inset) && Number.isFinite(strip)) {
      return { strip, inset, center };
    }
  }
  return { strip: STRIP_DEFAULT, inset: BARE_INSET, center: STRIP_DEFAULT / 2 };
}

/** Measures once per settled burst, with at most one request in flight. */
export function createWindowChromeRefresh(
  read: () => Promise<WindowChrome | null>,
  apply: (layout: ChromeLayout) => void,
) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let revision = 0;
  let measuring = false;
  let disposed = false;

  async function measure() {
    if (disposed || measuring || timer !== undefined) return;
    measuring = true;
    const startedAt = revision;
    try {
      const chrome = await read();
      if (!disposed && startedAt === revision) apply(chromeLayout(chrome));
    } catch {
      // Keep the last layout when IPC fails.
    } finally {
      measuring = false;
      if (timer === undefined && startedAt !== revision) void measure();
    }
  }

  return {
    measure,
    schedule() {
      if (disposed) return;
      revision++;
      clearTimeout(timer);
      timer = setTimeout(() => {
        timer = undefined;
        void measure();
      }, GEOMETRY_SETTLE_MS);
    },
    dispose() {
      disposed = true;
      clearTimeout(timer);
    },
  };
}
