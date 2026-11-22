import { Button, Modal } from "@mantine/core";
import {
  IconBrandApple, IconHelpCircle, IconPlayerPause, IconPlayerPlay, IconServer, IconUnlink,
} from "@tabler/icons-react";
import { useEffect, useState } from "react";
import type { SyncMode, SyncRules, SyncView } from "../../services/ipc/generated/core";
import { syncIpc } from "../../services/ipc/sync";
import { confirmAction } from "../../services/ipc/dialogs";
import { attempt } from "../../lib/operations";
import { refreshSyncs } from "../../state/store";
import { useMinute } from "../../lib/useNow";
import { formatInteger } from "../../lib/format";
import { SYNC_MODES, syncLine } from "../../lib/syncForm";
import { SyncFiles } from "./SyncFiles";
import { SyncFlow, SyncGuide } from "./SyncGuide";

/** A synced project's state and settings: status, conflicts, mode and files. */
export function SyncPanel({ view, opened, onClose }: { view: SyncView; opened: boolean; onClose: () => void }) {
  const { project, status } = view;
  const now = useMinute();
  const line = syncLine(status, now);
  const [mode, setMode] = useState<SyncMode>(project.mode);
  const [rules, setRules] = useState<SyncRules>(project.rules);
  const [guide, setGuide] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  useEffect(() => { if (opened) { setMode(project.mode); setRules(project.rules); } }, [opened, project.mode, project.rules]);
  const dirty = mode !== project.mode || JSON.stringify(rules) !== JSON.stringify(project.rules);

  async function run(key: string, task: () => Promise<unknown>, done?: string) {
    setBusy(key);
    await attempt(task(), done);
    await refreshSyncs();
    setBusy(null);
  }

  return (
    <>
      <Modal opened={opened} onClose={onClose} title={`Sync · ${project.name}`} size="xl" centered classNames={{ body: "sp" }}>
        <section className={`sp-status ${line.tone}`}>
          <div className="sp-ends">
            <span className="sp-end" title={project.hostPath}><IconBrandApple size={14} /><span className="truncate">{project.hostPath}</span></span>
            <SyncFlow mode={project.mode} compact />
            <span className="sp-end" title={project.guestPath}><IconServer size={14} /><span className="truncate">{project.guestPath}</span></span>
          </div>
          <div className="sp-line">
            <span className={`sp-dot ${line.tone}`} aria-hidden />
            <strong>{line.text}</strong>
            <span className="faint">
              {formatInteger(status.files)} paths{status.pushed || status.pulled ? ` · ${formatInteger(status.pushed)} sent, ${formatInteger(status.pulled)} received` : ""}
            </span>
            <span className="grow" />
            <Button size="xs" variant="default" leftSection={project.paused ? <IconPlayerPlay size={13} /> : <IconPlayerPause size={13} />} loading={busy === "pause"}
              onClick={() => void run("pause", () => syncIpc.update(project.id, { paused: !project.paused }))}>{project.paused ? "Resume" : "Pause"}</Button>
          </div>
        </section>

        {status.conflicts.length > 0 && (
          <section className="sp-conflicts">
            <h3>Changed on both sides</h3>
            <p className="faint">Neither version was overwritten. Choose which one to keep; the other is replaced.</p>
            <ul>
              {status.conflicts.map((path) => (
                <li key={path}>
                  <span className="mono truncate" title={path}>{path}</span>
                  <Button size="compact-xs" variant="default" loading={busy === `mac:${path}`}
                    onClick={() => void run(`mac:${path}`, () => syncIpc.resolve(project.id, path, "mac"))}>Keep Mac</Button>
                  <Button size="compact-xs" variant="default" loading={busy === `vm:${path}`}
                    onClick={() => void run(`vm:${path}`, () => syncIpc.resolve(project.id, path, "machine"))}>Keep machine</Button>
                </li>
              ))}
            </ul>
          </section>
        )}

        <section className="sp-settings">
          <div className="np-section-head">
            <strong>Changes flow</strong>
            <button type="button" className="np-guide-link" onClick={() => setGuide(true)}><IconHelpCircle size={13} /> How sync works</button>
          </div>
          <div className="np-modes" role="radiogroup" aria-label="Sync mode">
            {SYNC_MODES.map((info) => (
              <button key={info.mode} type="button" role="radio" aria-checked={mode === info.mode} className="np-mode" onClick={() => setMode(info.mode)}>
                <SyncFlow mode={info.mode} compact />
                <strong>{info.label}</strong>
                <span>{info.agentEdits}</span>
              </button>
            ))}
          </div>
          <SyncFiles hostPath={project.hostPath} mode={mode} rules={rules} onRules={setRules} />
        </section>

        <footer className="np-footer">
          <Button variant="subtle" color="red" leftSection={<IconUnlink size={14} />} onClick={async () => {
            if (!await confirmAction(`Stop syncing “${project.name}”? Files stay on your Mac and in the machine; they just stop being kept in step.`, "Stop syncing")) return;
            await run("remove", () => syncIpc.remove(project.id), "Sync stopped");
            onClose();
          }}>Stop syncing</Button>
          <span className="grow" />
          <Button variant="default" onClick={onClose}>Close</Button>
          <Button disabled={!dirty} loading={busy === "save"} onClick={() => void run("save", () => syncIpc.update(project.id, { mode, rules }), "Sync settings saved")}>
            Save changes
          </Button>
        </footer>
      </Modal>
      <SyncGuide opened={guide} onClose={() => setGuide(false)} />
    </>
  );
}

/** The project's sync state as one dot in the sidebar. */
export function SyncDot({ view, onClick }: { view: SyncView; onClick: () => void }) {
  const { tone, text } = syncLine(view.status);
  const transferring = tone === "busy" && view.status.state === "syncing";
  const [showTransfer, setShowTransfer] = useState(false);
  useEffect(() => {
    if (!transferring) {
      setShowTransfer(false);
      return;
    }
    const timer = setTimeout(() => setShowTransfer(true), 500);
    return () => clearTimeout(timer);
  }, [transferring]);
  if (tone === "ok" || tone === "off" || (tone === "busy" && !(transferring && showTransfer))) return null;
  return (
    <button type="button" className="press grid size-5 shrink-0 cursor-pointer place-items-center rounded-md border-0 bg-transparent p-0 hover:bg-hover"
      title={text} aria-label={`Sync: ${text}`} onClick={(event) => { event.stopPropagation(); onClick(); }}>
      <span className={`size-1.5 rounded-full ${tone === "busy" ? "animate-breathe bg-accent" : tone === "bad" ? "bg-bad" : "bg-warn"}`} aria-hidden />
    </button>
  );
}
