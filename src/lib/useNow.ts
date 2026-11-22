import { useEffect, useState } from "react";

/** The current time on `read`'s clock (the Mac's by default; pass
 * `guestNow` to time against guest timestamps), refreshed every second while
 * `active`. */
export function useNow(active: boolean, read: () => number = Date.now): number {
  const [now, setNow] = useState(read);
  useEffect(() => {
    if (!active) return;
    setNow(read());
    const timer = window.setInterval(() => setNow(read()), 1000);
    return () => window.clearInterval(timer);
  }, [active, read]);
  return active ? now : read();
}

/** Wall-clock time refreshed every half minute; for relative labels such as
 * "5m" that should not freeze while the view is open. */
export function useMinute(): number {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, []);
  return now;
}
