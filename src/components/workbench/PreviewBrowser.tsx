import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { IconArrowLeft, IconArrowRight, IconBrandChrome, IconHome, IconServer, IconWorld, IconX } from "@tabler/icons-react";
import { useEffect, useRef, useState } from "react";
import type { ListeningPort } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { chromeIpc } from "../../services/ipc/chrome";
import { workbenchIpc } from "../../services/ipc/workbench";
import { parsePreviewTarget, previewAddress, type PreviewTarget } from "../../lib/workbench";
import { uiActions } from "../../state/ui";

/**
 * A browser for the VM's own dev servers. Each port is reached through an SSH
 * forward the Mac keeps open (`preview_forward`), and shown in an iframe, the
 * way an editor's simple browser previews localhost. Back and forward walk
 * the frame's own history (the window's joint session history); the page
 * itself is cross-origin, so its address is the one you navigated to.
 */
export function PreviewBrowser({ visible }: { visible: boolean }) {
  const [target, setTarget] = useState<PreviewTarget | null>(null);
  const [base, setBase] = useState<string | null>(null);
  const [address, setAddress] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [nonce, setNonce] = useState(0);
  const [ports, setPorts] = useState<ListeningPort[] | null>(null);
  const [depth, setDepth] = useState({ back: 0, forward: 0 });
  const frame = useRef<HTMLIFrameElement>(null);
  const pending = useRef<"nav" | "history" | "reload" | null>(null);

  useAutoRefresh(async (isCurrent) => {
    try {
      const next = await workbenchIpc.ports();
      if (isCurrent()) setPorts(next);
    } catch {
      if (isCurrent()) setPorts((value) => value ?? []);
    }
  }, { enabled: visible, interval: 4_000 });

  const port = target?.port;
  const isListening = ports?.some((item) => item.port === port);
  const wasListening = useRef<boolean | undefined>(undefined);
  useEffect(() => { wasListening.current = undefined; }, [port]);
  useEffect(() => {
    if (!port || !visible || isListening == null) return;
    if (isListening && wasListening.current === false) {
      pending.current = "reload";
      setNonce((n) => n + 1);
    }
    wasListening.current = isListening;
  }, [port, visible, isListening]);

  async function go(next: PreviewTarget) {
    setError(null);
    setLoading(true);
    if (base && target?.port === next.port && target.path === next.path) {
      pending.current = "reload";
      setAddress(previewAddress(next));
      setNonce((n) => n + 1);
      return;
    }
    try {
      const forward = await workbenchIpc.forward(next.port);
      const fresh = target?.port !== next.port;
      setBase(forward.url.replace(/\/$/, ""));
      setTarget(next);
      setAddress(previewAddress(next));
      pending.current = "nav";
      if (fresh) setDepth({ back: 0, forward: 0 });
      else setDepth((d) => ({ back: d.back + 1, forward: 0 }));
    } catch (err) {
      setLoading(false);
      setError(toIpcError(err).message);
    }
  }

  function submit() {
    const next = parsePreviewTarget(address, target?.port ?? null);
    if (!next) { uiActions.toast("Enter a port or a localhost address on the machine, e.g. 3000 or localhost:5173/docs", "info"); return; }
    void go(next);
  }

  function onLoad() {
    setLoading(false);
    const cause = pending.current;
    pending.current = null;
    // A load we did not start is a link the page followed.
    if (cause == null) setDepth((d) => ({ back: d.back + 1, forward: 0 }));
  }

  function history(step: -1 | 1) {
    pending.current = "history";
    setLoading(true);
    setDepth((d) => (step < 0 ? { back: d.back - 1, forward: d.forward + 1 } : { back: d.back + 1, forward: d.forward - 1 }));
    window.history.go(step);
  }

  function close() {
    if (target) void workbenchIpc.closeForward(target.port).catch(() => undefined);
    setTarget(null); setBase(null); setAddress(""); setError(null); setLoading(false);
  }

  const src = base && target ? `${base}${target.path}` : null;

  return (
    <div className="wb-browser">
      <form className="wb-urlbar" onSubmit={(e) => { e.preventDefault(); submit(); }}>
        <button type="button" className="wb-icon-btn" title="Back" disabled={!target || depth.back <= 0} onClick={() => history(-1)}><IconArrowLeft size={15} /></button>
        <button type="button" className="wb-icon-btn" title="Forward" disabled={!target || depth.forward <= 0} onClick={() => history(1)}><IconArrowRight size={15} /></button>
        <div className="wb-address">
          <IconWorld size={13} className="faint" />
          <input value={address} onChange={(e) => setAddress(e.currentTarget.value)} placeholder="Port or localhost address in the machine" spellCheck={false} />
        </div>
        {target && <button type="button" className="wb-icon-btn" title="Open in the machine's Chrome" onClick={() => void chromeIpc.open(`http://localhost:${target.port}${target.path}`).then(() => uiActions.navigate({ kind: "chrome" }), (err) => uiActions.toast(toIpcError(err).message, "error"))}><IconBrandChrome size={15} /></button>}
        {target && <button type="button" className="wb-icon-btn" title="Close preview" onClick={close}><IconHome size={15} /></button>}
      </form>
      {error && <div className="wb-banner"><span className="grow">{error}</span><button className="wb-icon-btn" onClick={() => setError(null)}><IconX size={13} /></button></div>}
      {src ? (
        <iframe key={`${target!.port}:${nonce}`} ref={frame} className="wb-frame" src={src} title="Preview" aria-busy={loading} onLoad={onLoad}
          sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-modals allow-downloads" />
      ) : (
        <div className="wb-scroll wb-ports">
          <div className="wb-ports-head"><IconServer size={15} /><strong>Dev servers in the machine</strong></div>
          {ports == null && <div className="wb-empty faint">Looking for servers…</div>}
          {ports?.length === 0 && (
            <div className="wb-empty">
              <span>No servers are listening yet.</span>
              <span className="faint">Start one (for example <code>npm run dev</code>) or ask the agent to; it appears here by itself.</span>
            </div>
          )}
          {ports?.map((port) => (
            <button key={port.port} className="wb-port" onClick={() => void go({ port: port.port, path: "/" })} title={port.command ?? undefined}>
              <span className="wb-port-dot" />
              <span className="wb-port-num mono">:{port.port}</span>
              <span className="wb-port-proc truncate">{port.process ?? "unknown process"}</span>
              <span className="wb-tree-dir truncate">{port.command}</span>
              <span className="wb-chip">Open</span>
            </button>
          ))}
          <p className="wb-hint faint">Pages that forbid framing (X-Frame-Options) can be opened in the machine's Chrome instead.</p>
        </div>
      )}
    </div>
  );
}
