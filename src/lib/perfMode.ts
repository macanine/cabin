// The interface's effects level. "lite" turns off glass blur, motion, shadows
// and the window's vibrancy for slow machines; "auto" follows the system's
// reduced-motion / reduced-transparency settings. Frontend-only, per Mac.
export type PerfPref = "auto" | "full" | "lite";
const KEY = "cabin.perf";

export function readPerf(): PerfPref {
  try {
    const value = localStorage.getItem(KEY);
    return value === "full" || value === "lite" ? value : "auto";
  } catch {
    return "auto";
  }
}

const queries = () => ["(prefers-reduced-motion: reduce)", "(prefers-reduced-transparency: reduce)"].map((q) => window.matchMedia?.(q));

/** The level actually in effect for a preference. */
export function effectivePerf(pref: PerfPref): "full" | "lite" {
  if (pref !== "auto") return pref;
  return queries().some((q) => q?.matches) ? "lite" : "full";
}

const MAC = typeof navigator !== "undefined" && /Mac/.test(navigator.userAgent);

/** Put the level on the root element; lite also drops the window vibrancy. */
export function applyPerf(pref: PerfPref = readPerf()) {
  const level = effectivePerf(pref);
  const root = document.documentElement;
  root.dataset.perf = level;
  if (MAC) {
    if (level === "lite") delete root.dataset.vibrancy;
    else root.dataset.vibrancy = "";
  }
}

export function setPerf(pref: PerfPref) {
  try { localStorage.setItem(KEY, pref); } catch { /* applies for this run only */ }
  applyPerf(pref);
}

/** Apply at start and follow system changes while the preference is "auto". */
export function initPerf() {
  applyPerf();
  for (const q of queries()) q?.addEventListener("change", () => { if (readPerf() === "auto") applyPerf(); });
}
