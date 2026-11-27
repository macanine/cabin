import { useCallback, useEffect, useRef } from "react";
import { IS_MAC } from "../lib/platform";
import { chromeIpc, type DisplayProtocol } from "../services/ipc/chrome";
import { toIpcError } from "../services/ipc/common";
import { clipboard } from "../services/ipc/dialogs";

const RETRY_DELAYS = [400, 1000, 2500];
/** After the quick retries, keep trying in the background at this pace. */
const BACKGROUND_RETRY = 8000;
const CONNECT_TIMEOUT = 8000;

export type ViewerState = { phase: "connecting" | "connected" } | { phase: "failed"; message: string };

export function ChromeViewer({ protocol, onState, onError }: {
  protocol: DisplayProtocol;
  onState: (state: ViewerState) => void;
  onError: (message: string) => void;
}) {
  const target = useRef<HTMLDivElement>(null);
  const paste = useCallback(() => {
    void clipboard.read().then(chromeIpc.paste).catch((cause) => onError(toIpcError(cause).message));
  }, [onError]);

  useEffect(() => {
    const element = target.current;
    if (!element) return;
    let close = () => {};
    let retryTimer: number | undefined;
    let failures = 0;

    const connect = () => {
      const controller = new AbortController();
      const { signal } = controller;
      let dispose = () => {};
      let connectedAt: number | undefined;
      const timeout = window.setTimeout(() => fail("The display took too long to connect."), CONNECT_TIMEOUT);
      const quiet = failures >= RETRY_DELAYS.length;
      close = () => {
        if (signal.aborted) return;
        controller.abort();
        window.clearTimeout(timeout);
        dispose();
      };
      // Background retries keep the "failed" message up until one succeeds.
      if (!quiet) onState({ phase: "connecting" });

      function ready() {
        if (signal.aborted || connectedAt !== undefined) return;
        connectedAt = performance.now();
        window.clearTimeout(timeout);
        onState({ phase: "connected" });
      }

      function fail(message: string) {
        if (signal.aborted) return;
        close();
        if (connectedAt !== undefined && performance.now() - connectedAt > 30_000) failures = 0;
        const delay = failures < RETRY_DELAYS.length ? RETRY_DELAYS[failures++] : undefined;
        if (delay === undefined) {
          // Say so, but keep trying: the display usually returns on its own
          // (Chrome restarting, the machine reconnecting).
          onState({ phase: "failed", message });
          retryTimer = window.setTimeout(connect, BACKGROUND_RETRY);
        } else {
          onState({ phase: "connecting" });
          retryTimer = window.setTimeout(connect, delay);
        }
      }

      const open = async () => {
        const display = await chromeIpc.display(protocol);
        if (signal.aborted) return;
        if (display.protocol !== protocol) throw new Error("The machine returned a different display protocol.");
        if (protocol === "vnc") {
          const { default: RFB } = await import("@novnc/novnc");
          if (signal.aborted) return;
          const client = new RFB(element, display.url, { shared: true });
          dispose = () => client.disconnect();
          client.scaleViewport = true;
          client.background = "transparent";
          client.qualityLevel = 6;
          client.compressionLevel = 1;
          client.addEventListener("connect", ready);
          client.addEventListener("disconnect", () => fail("The VNC display disconnected."));
        } else {
          const frame = document.createElement("iframe");
          const origin = new URL(display.url).origin;
          frame.title = "Chrome desktop";
          frame.setAttribute("sandbox", "allow-scripts allow-same-origin");
          frame.referrerPolicy = "no-referrer";
          window.addEventListener("message", (event) => {
            if (event.origin !== origin || event.source !== frame.contentWindow || event.data?.type !== "cabin-spice") return;
            const { state, message } = event.data;
            if (state === "ready") ready();
            else if (state === "paste") paste();
            else if (state === "error" || state === "disconnected") {
              fail(typeof message === "string" ? message : "The SPICE display disconnected.");
            }
          }, { signal });
          dispose = () => frame.remove();
          frame.src = display.url;
          element.append(frame);
        }
      };
      void open().catch((cause) => fail(toIpcError(cause).message));
    };

    connect();
    return () => {
      window.clearTimeout(retryTimer);
      close();
    };
  }, [protocol, onState, paste]);

  return <div ref={target} className="chrome-viewer" role="application" aria-label="Chrome desktop" onKeyDownCapture={(event) => {
    const pasteKey = IS_MAC
      ? event.metaKey && !event.ctrlKey && !event.altKey
      : event.ctrlKey && !event.metaKey && !event.altKey;
    if (!pasteKey || event.key.toLowerCase() !== "v") return;
    event.preventDefault();
    event.stopPropagation();
    paste();
  }} />;
}
