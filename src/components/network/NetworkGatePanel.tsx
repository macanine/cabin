import { Button, Loader, Switch } from "@mantine/core";
import {
  IconAlertTriangle, IconArrowRight, IconCheck, IconChevronRight, IconHelpCircle,
  IconPlayerPause, IconShieldCheck, IconShieldLock,
} from "@tabler/icons-react";
import { useState, type ReactNode } from "react";
import { attempt } from "../../lib/operations";
import { checkFix, networkGuidance, selectNetworkSummary, splitChecks, type FixTarget } from "../../lib/networkSummary";
import type { NetworkCheckItem, NetworkCheckReport, NetworkCheckStatus, NetworkStatus } from "../../services/ipc/generated/core";
import { confirmAction } from "../../services/ipc/dialogs";
import { networkIpc } from "../../services/ipc/network";
import { useApp } from "../../state/store";
import { uiActions } from "../../state/ui";
import { RecoveryPanel } from "../Recovery";
import { DisclosureChevron, Reveal } from "../motion";

export function NetworkCheckButton() {
  return (
    <button type="button" className="press inline-flex cursor-pointer items-center gap-1.5 rounded-md border-0 bg-transparent py-1 text-sm font-medium text-muted hover:text-fg" aria-haspopup="dialog" onClick={uiActions.openNetworkCheck}>
      Network check <IconChevronRight size={14} aria-hidden />
    </button>
  );
}

const CHECK_ICON: Record<NetworkCheckStatus, React.ReactNode> = {
  pass: <IconCheck size={12} stroke={3} />,
  fail: <IconAlertTriangle size={13} />,
  unknown: <IconHelpCircle size={13} />,
  skipped: <span className="size-1.5 rounded-full bg-current" />,
} as Record<NetworkCheckStatus, React.ReactNode>;

function Evidence({ check }: { check: NetworkCheckItem }) {
  if (!check.evidence.length) return null;
  return (
    <details className="group mt-1.5 text-xs">
      <summary className="inline-flex cursor-pointer list-none items-center gap-1 text-faint hover:text-muted [&::-webkit-details-marker]:hidden">
        <IconChevronRight size={11} className="transition-transform duration-200 group-open:rotate-90" />Details
      </summary>
      <dl className="mt-1.5 mb-0 grid gap-1">
        {check.evidence.map((entry, index) => (
          <div key={`${entry.label}-${index}`} className="grid grid-cols-[minmax(90px,max-content)_minmax(0,1fr)] gap-3">
            <dt className="text-faint">{entry.label}</dt>
            <dd className="m-0 font-mono [overflow-wrap:anywhere]">{entry.value}</dd>
          </div>
        ))}
      </dl>
    </details>
  );
}

type CheckAction = (target: FixTarget) => void;

/** A check that needs attention: what failed, why, and the one fix. */
function ProblemCard({ check, onFix, note }: { check: NetworkCheckItem; onFix?: CheckAction; note?: boolean }) {
  const fix = checkFix(check);
  const bad = check.status === "fail" && !note;
  return (
    <li className={`flex items-start gap-3 rounded-xl border bg-surface/70 p-3 ${bad ? "border-bad/35" : "border-line"}`}>
      <span className={`mt-0.5 grid size-[22px] shrink-0 place-items-center rounded-full ${bad ? "bg-bad/13 text-bad" : "bg-warn/13 text-warn"}`} aria-hidden="true">{CHECK_ICON[check.status]}</span>
      <div className="min-w-0 flex-1">
        <strong className="text-md">{check.title}</strong>
        <p className="m-0 mt-0.5 text-sm text-muted">{check.summary}</p>
        <Evidence check={check} />
      </div>
      {onFix && fix && (
        <Button size="compact-xs" variant="light" rightSection={<IconArrowRight size={12} />} onClick={() => onFix(fix.target)}>{fix.label}</Button>
      )}
    </li>
  );
}

/**
 * A full check, simplified: blockers first (each with its one fix), optional
 * notes after, and every passed check folded into one line.
 */
export function NetworkCheckReportView({ report, onFix }: { report: NetworkCheckReport; onFix?: CheckAction }) {
  const { problems, notes, passed } = splitChecks(report);
  const [open, setOpen] = useState(false);
  return (
    <div className="grid gap-2.5">
      {problems.length > 0 && (
        <ol className="m-0 grid list-none gap-2 p-0" aria-label="Blocking internet access">
          {problems.map((check) => <ProblemCard key={check.id} check={check} onFix={onFix} />)}
        </ol>
      )}
      {notes.length > 0 && (
        <ol className="m-0 grid list-none gap-2 p-0" aria-label="Optional checks">
          {notes.map((check) => <ProblemCard key={check.id} check={check} onFix={onFix} note />)}
        </ol>
      )}
      {passed.length > 0 && (
        <div className="overflow-hidden rounded-xl border border-line bg-surface/60">
          <button type="button" aria-expanded={open} onClick={() => setOpen(!open)} className="press flex w-full cursor-pointer items-center gap-3 border-0 bg-transparent px-3 py-2.5 text-left text-sm text-fg hover:bg-hover">
            <span className="grid size-[22px] shrink-0 place-items-center rounded-full bg-ok text-white" aria-hidden="true"><IconCheck size={12} stroke={3} /></span>
            <span className="min-w-0 flex-1">{passed.length} {passed.length === 1 ? "check" : "checks"} passed</span>
            <DisclosureChevron open={open} size={13} className="text-faint" />
          </button>
          <Reveal open={open}>
            <ul className="m-0 grid list-none gap-1.5 p-0 pr-3 pb-3 pl-[46px]">
              {passed.map((check) => (
                <li key={check.id} title={check.summary} className="grid min-w-0 text-sm">
                  <span>{check.title}</span>
                  <small className="truncate text-xs text-faint">{check.summary}</small>
                </li>
              ))}
            </ul>
          </Reveal>
        </div>
      )}
    </div>
  );
}

export function NetworkGatePanel({ status, onRemediate, history }: {
  status: NetworkStatus;
  onRemediate: CheckAction;
  history: ReactNode;
}) {
  const summary = useApp(selectNetworkSummary);
  const machine = useApp((state) => state.machine);
  const [busy, setBusy] = useState<"verify" | "accept" | "pause" | "recover" | null>(null);
  const report = status.report?.revision === status.revision ? status.report : null;
  const connected = machine?.state === "running" && machine.connection === "connected";
  const canChange = connected && status.available && status.running && busy == null;
  const canVerify = canChange && !status.paused && status.gate !== "checking";
  const checking = busy === "verify" || busy === "accept" || summary.tone === "busy";
  const exitProblem = summary.blocker?.id === "exit";
  const fix = summary.blocker && checkFix(summary.blocker);

  async function run(action: "verify" | "accept" | "pause" | "recover") {
    if (action === "accept" && !await confirmAction(
      "Replace this route’s approved exit if the current exit passes a full network check? Only continue if you expected the change.",
      "Approve and check", "Approve new exit",
    )) return;
    setBusy(action);
    try {
      if (action === "verify" || action === "accept") await attempt(networkIpc.verify(action === "accept"));
      else if (action === "pause") await attempt(networkIpc.setPaused(!status.paused));
      else await attempt(networkIpc.setAutoRecover(!status.autoRecover));
    } finally {
      setBusy(null);
    }
  }

  if (!connected) {
    return (
      <p className="m-0 py-2 text-sm text-muted" role="status">{machine?.state === "running" ? "Waiting for the machine to connect." : "Start the machine to check internet access."}</p>
    );
  }

  const guidance = networkGuidance(status, summary);
  const bannerTone = checking ? "info" : status.paused ? "warn" : summary.tone === "ok" ? "ok" : summary.tone === "bad" ? "bad" : "off";
  const detailLine = status.exitPolicyError ?? (summary.tone === "bad" && summary.detail !== guidance.headline ? summary.detail : null);

  return (
    <section className="grid gap-4" aria-label="Network check">
      <div className={`net-guide-card ${bannerTone}`} role="status">
        <div className="net-guide-head">
          <span className="net-guide-icon" aria-hidden>
            {checking ? <Loader size={14} /> : status.paused ? <IconPlayerPause size={16} /> : summary.tone === "ok" ? <IconShieldCheck size={16} /> : <IconShieldLock size={16} />}
          </span>
          <div className="min-w-0 flex-1">
            <h2 className="m-0 text-[15px] leading-snug font-semibold text-fg">{guidance.headline}</h2>
            <p className="m-0 mt-1 text-sm leading-relaxed text-muted">{guidance.explanation}</p>
          </div>
        </div>
        {detailLine && !checking && (
          <div className="net-guide-detail mono">{detailLine}</div>
        )}
        {!checking && status.available && (status.running || status.paused) && (
          <div className="flex flex-wrap items-center gap-2 pt-0.5">
            {status.paused ? (
              <Button size="xs" disabled={!canChange} loading={busy === "pause"} onClick={() => void run("pause")}>Resume internet</Button>
            ) : status.exitPolicyError ? (
              <>
                <Button size="xs" disabled={!canVerify} onClick={() => void run("accept")}>Approve new exit</Button>
                <Button size="xs" variant="default" onClick={() => onRemediate("route")}>Review route &amp; IP policy</Button>
              </>
            ) : (
              <>
                {fix && <Button size="xs" onClick={() => onRemediate(fix.target)}>{exitProblem ? "Review route" : fix.label}</Button>}
                <Button size="xs" variant={fix || summary.tone === "ok" ? "default" : "filled"} disabled={!canVerify} onClick={() => void run("verify")}>Check again</Button>
                {summary.tone === "ok" && <Button size="xs" variant="subtle" color="gray" disabled={!canChange} loading={busy === "pause"} onClick={() => void run("pause")}>Pause internet</Button>}
              </>
            )}
          </div>
        )}
        {guidance.tip && !checking && (
          <p className="m-0 text-xs leading-relaxed text-faint">{guidance.tip}</p>
        )}
      </div>

      <details className="group border-t border-line pt-2.5" open={summary.tone === "bad" && !status.exitPolicyError && Boolean(report)}>
        <summary className="flex cursor-pointer list-none items-center justify-between gap-2 rounded-md px-0.5 py-1 text-sm font-medium text-muted hover:text-fg [&::-webkit-details-marker]:hidden">
          Diagnostics and settings <IconChevronRight size={15} className="transition-transform duration-150 group-open:rotate-90" />
        </summary>
        <div className="mt-3 grid gap-3">
          {status.error && <RecoveryPanel compact message={status.error} />}
          {report && <NetworkCheckReportView key={report.id} report={report} onFix={onRemediate} />}
          {history}
          {status.available && <div className="flex flex-wrap gap-x-5 gap-y-3 pt-1">
            {!status.paused && summary.tone !== "ok" && <Switch size="xs" label="Pause internet" checked={status.paused} disabled={!canChange} onChange={() => void run("pause")} />}
            <Switch size="xs" label="Automatically retry" checked={status.autoRecover} disabled={!canChange} onChange={() => void run("recover")} />
          </div>}
        </div>
      </details>
    </section>
  );
}
