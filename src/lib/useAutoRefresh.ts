import { useEffect, useRef } from "react";

/** Refresh a mounted view on entry and foreground return. Optional polling
 * is visible-only, and a slow request never overlaps the next one. */
export function useAutoRefresh(read: (isCurrent: () => boolean) => void | Promise<unknown>, {
  enabled = true, initial = true, interval = 0, minGap = 1_000, refreshKey,
}: { enabled?: boolean; initial?: boolean; interval?: number; minGap?: number; refreshKey?: string } = {}) {
  const latest = useRef(read);
  latest.current = read;
  useEffect(() => {
    if (!enabled) return;
    let active = true;
    let running = false;
    let pendingForeground = false;
    let foregroundTimer: number | undefined;
    let last = initial ? -Infinity : performance.now();
    const wait = () => minGap - (performance.now() - last);

    function onForeground() {
      if (!active || document.hidden) return;
      pendingForeground = true;
      if (running) return;
      window.clearTimeout(foregroundTimer);
      // Focus and visibility can arrive together. Keep one read even if the
      // return comes during another request or inside its minimum gap.
      foregroundTimer = window.setTimeout(refresh, Math.max(50, Math.ceil(wait())));
    }

    async function refresh() {
      if (!active || running || document.hidden) return;
      if (wait() > 0) {
        if (pendingForeground) onForeground();
        return;
      }
      window.clearTimeout(foregroundTimer);
      pendingForeground = false;
      running = true;
      last = performance.now();
      try { await latest.current(() => active); }
      catch { /* The reader owns its error and stale data. */ }
      finally {
        running = false;
        if (pendingForeground) onForeground();
      }
    }

    if (initial) void refresh();
    window.addEventListener("focus", onForeground);
    document.addEventListener("visibilitychange", onForeground);
    const timer = interval > 0 ? window.setInterval(refresh, interval) : undefined;
    return () => {
      active = false;
      window.removeEventListener("focus", onForeground);
      document.removeEventListener("visibilitychange", onForeground);
      window.clearTimeout(foregroundTimer);
      window.clearInterval(timer);
    };
  }, [enabled, initial, interval, minGap, refreshKey]);
}
