import { Button } from "@mantine/core";
import { IconAlertTriangle, IconCheck, IconCircleX } from "@tabler/icons-react";
import { useState } from "react";
import type { Fix, Readiness } from "../../services/ipc/generated/common";
import { machineIpc } from "../../services/ipc/app";
import { toIpcError } from "../../services/ipc/common";
import { DisclosureChevron, Reveal } from "../../components/motion";
import { Note } from "../../components/Boot";
import { readinessView } from "../../lib/installForm";

/** The button for a fix, or null when the only remedy is to reinstall. */
export function fixLabel(fix: Fix): string | null {
  switch (fix.kind) {
    case "enableWhpx": return "Turn on";
    case "restartHost": return "Restart Windows";
    case "openUrl": return "How to fix";
    case "reinstallApp": return null;
  }
}

function Row({ item, tone, busy, onFix }: { item: Readiness; tone: "bad" | "warn"; busy: boolean; onFix: (fix: Fix) => void }) {
  const label = item.fix ? fixLabel(item.fix) : null;
  return (
    <div className="flex items-start gap-3 py-2.5">
      {tone === "bad" ? <IconCircleX size={17} className="mt-0.5 shrink-0 text-bad" /> : <IconAlertTriangle size={17} className="mt-0.5 shrink-0 text-warn" />}
      <div className="grid min-w-0 flex-1 gap-0.5">
        <strong className="text-md">{item.label}</strong>
        <span className="text-sm text-muted">{item.detail}</span>
      </div>
      {item.fix && label && <Button size="compact-sm" variant="default" loading={busy} onClick={() => onFix(item.fix!)}>{label}</Button>}
    </div>
  );
}

/**
 * What this computer needs before installing: blockers in red and warnings in
 * amber, each with its fix button; everything that passed folds into one line.
 */
export function ReadinessPanel({ items, onChanged }: { items: Readiness[] | null; onChanged: () => void }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [restart, setRestart] = useState<"needed" | "confirm" | null>(null);
  const [showPassed, setShowPassed] = useState(false);
  if (items == null) return <Note>Checking this computer…</Note>;
  const { blocking, warnings, passed } = readinessView(items);

  async function fix(next: Fix, id: string) {
    setBusy(id);
    setError(null);
    try {
      const outcome = await machineIpc.fix(next);
      if (outcome.restartRequired) setRestart("needed");
      onChanged();
    } catch (err) {
      setError(toIpcError(err).message);
    } finally {
      setBusy(null);
    }
  }

  const problems = [...blocking.map((item) => ({ item, tone: "bad" as const })), ...warnings.map((item) => ({ item, tone: "warn" as const }))];
  return (
    <div className="grid gap-2">
      {problems.length > 0 && (
        <div role={blocking.length ? "alert" : "status"} className={`divide-y divide-line/60 rounded-xl border px-4 py-1 ${blocking.length ? "border-bad/40 bg-bad/5" : "border-warn/40 bg-warn/5"}`}>
          {problems.map(({ item, tone }) => (
            <Row key={item.id} item={item} tone={tone} busy={busy === item.id} onFix={(f) => void fix(f, item.id)} />
          ))}
        </div>
      )}
      {restart && (
        <Note tone="warn">
          <span className="flex flex-wrap items-center justify-between gap-3">
            <span>Windows needs a restart to finish turning this on. Save your work first.</span>
            {restart === "needed"
              ? <Button size="compact-sm" variant="default" onClick={() => setRestart("confirm")}>Restart Windows</Button>
              : <Button size="compact-sm" color="red" loading={busy === "restart"} onClick={() => void fix({ kind: "restartHost" }, "restart")}>Restart now</Button>}
          </span>
        </Note>
      )}
      {error && <Note tone="bad" role="alert">{error}</Note>}
      {passed.length > 0 && (
        <div>
          <button type="button" aria-expanded={showPassed} onClick={() => setShowPassed((v) => !v)}
            className="press flex w-full cursor-pointer items-center gap-2 border-0 bg-transparent px-1 py-1 text-left text-sm text-muted">
            <IconCheck size={15} className="text-ok" />
            <span className="flex-1">{problems.length === 0 ? "This computer is ready" : `${passed.length} ${passed.length === 1 ? "check" : "checks"} passed`}</span>
            <DisclosureChevron open={showPassed} />
          </button>
          <Reveal open={showPassed}>
            <ul className="m-0 grid list-none gap-1 px-1 pt-1 pb-2 text-sm text-muted">
              {passed.map((item) => <li key={item.id} className="flex gap-2"><IconCheck size={14} className="mt-0.5 shrink-0 text-ok" /><span><strong className="text-fg">{item.label}</strong> · {item.detail}</span></li>)}
            </ul>
          </Reveal>
        </div>
      )}
    </div>
  );
}
