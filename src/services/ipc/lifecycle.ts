import { invokeIpc } from "./common";

/** App lifecycle (src-tauri/src/lifecycle.rs): quit, hide to the tray, and the quit request. */
export const lifecycleIpc = {
  /** Leave the app; `stopMachine` stops the VM first. */
  quit: (stopMachine: boolean) => invokeIpc<void>("app_quit", { stopMachine }),
  /** Hide the window; the app and the machine keep running. */
  background: () => invokeIpc<void>("app_background"),
  /** Called when ⌘Q or the tray's Quit asks to leave. Returns the unsubscribe. */
  onQuitRequested(handler: () => void): () => void {
    let off: (() => void) | null = null;
    let gone = false;
    import("@tauri-apps/api/event")
      .then(({ listen }) => listen("lifecycle://quit-requested", handler))
      .then((unlisten) => { if (gone) unlisten(); else off = unlisten; })
      .catch(() => undefined);
    return () => { gone = true; off?.(); };
  },
};
