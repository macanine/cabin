import { TauriEvent } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invokeIpc } from "./common";
import type { WindowChrome } from "./generated/common";

export type { WindowChrome };

/** Where the native window controls (macOS traffic lights) are, or null. */
export function windowChrome(): Promise<WindowChrome | null> {
  return invokeIpc<WindowChrome | null>("window_chrome");
}

/** Full-screen transitions also emit a resize when they finish. */
export async function onWindowGeometryChange(onChange: () => void): Promise<() => void> {
  const window = getCurrentWindow();
  const listeners = await Promise.allSettled([
    window.onResized(onChange),
    window.onScaleChanged(onChange),
    window.onThemeChanged(onChange),
    // Bringing a hidden or minimized window back can restore its title bar.
    window.listen(TauriEvent.WINDOW_FOCUS, onChange),
  ]);
  return () => {
    for (const listener of listeners) {
      if (listener.status === "fulfilled") listener.value();
    }
  };
}
