// What to do when the app is asked to quit while the machine runs.
export type QuitPref = "ask" | "stop" | "background";
const KEY = "cabin.quit";

export function readQuitPref(): QuitPref {
  try {
    const value = localStorage.getItem(KEY);
    return value === "stop" || value === "background" ? value : "ask";
  } catch {
    return "ask";
  }
}

export function writeQuitPref(pref: QuitPref) {
  try {
    if (pref === "ask") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, pref);
  } catch { /* asks again next time */ }
}
