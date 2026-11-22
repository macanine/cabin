import { Button, Loader, Switch } from "@mantine/core";
import {
  IconAlertTriangle, IconArrowRight, IconCheck, IconChevronRight, IconCloudOff, IconHelpCircle,
  IconPlayerPause, IconShieldCheck,
} from "@tabler/icons-react";
import { useState } from "react";
import { attempt } from "../../lib/operations";
import { checkFix, selectNetworkSummary, splitChecks, type FixTarget, type NetTone } from "../../lib/networkSummary";
import type { NetworkCheckItem, NetworkCheckReport, NetworkCheckStatus, NetworkStatus } from "../../services/ipc/generated/core";
import { networkIpc } from "../../services/ipc/network";
import { useApp } from "../../state/store";
import { uiActions } from "../../state/ui";
import { RecoveryPanel } from "../Recovery";
import { DisclosureChevron, Reveal } from "../motion";

const time = new Intl.DateTimeFormat("en-US", { hour: "numeric", minute: "2-digit" });

const TONE_STYLE: Record<NetTone, string> = {
  ok: "bg-ok/13 text-ok",
  busy: "bg-accent/12 text-accent",
  warn: "bg-warn/13 text-warn",
  bad: "bg-bad/13 text-bad",
  off: "bg-hover text-muted",
};

export function AccessMark({ tone, size = 18 }: { tone: NetTone; size?: number }) {
  return (
    <span className={`grid shrink-0 place-items-center rounded-xl ${TONE_STYLE[tone]}`} style={{ width: size * 1.8, height: size * 1.8 }} aria-hidden="true">
      <span key={tone} className={`grid place-items-center ${tone === "busy" ? "" : "animate-fade"}`}>
        {tone === "ok" ? <IconShieldCheck size={size} />
          : tone === "busy" ? <Loader size={size - 2} />
            : tone === "warn" ? <IconPlayerPause size={size} />
              : <IconCloudOff size={size} />}
      </span>
    </span>
  );
}

/** One line in the network page; opens the Network Check. */
export function NetworkGateSummary({ status }: { status: NetworkStatus }) {
  const summary = useApp(selectNetworkSummary);
  const report = status.report?.revision === status.revision ? status.report : null;
  const problems = report ? report.totalRequired - report.passedRequired : 0;
  return (
    <button type="button" className="glass press flex w-full cursor-pointer items-center gap-3 rounded-xl p-3 text-left text-fg hover:border-accent/40" aria-haspopup="dialog" onClick={uiActions.openNetworkCheck}>
      <AccessMark tone={summary.tone} size={16} />
      <span className="grid min-w-0 flex-1">
        <strong className="text-md">{summary.title}</strong>
        <span className="truncate text-sm text-muted">{summary.detail}</span>
      </span>
      {report && problems > 0 && <span className="shrink-0 rounded-full bg-bad/12 px-2 py-0.5 text-xs font-medium text-bad">{problems} to fix</span>}
      <IconChevronRight size={15} className="text-faint" aria-hidden="true" />
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

/**
 * Internet access for the machine: verification and recovery follow the gateway state,
 * the problems if any, and two switches. Nothing else.
 */
export function NetworkGatePanel({ status, onRemediate }: {
  status: NetworkStatus;
  onRemediate: CheckAction;
}) {
  const summary = useApp(selectNetworkSummary);
  const machineRunning = useApp((state) => state.machine?.state === "running");
  const loadError = useApp((state) => state.networkLoadError);
  const [busy, setBusy] = useState<"verify" | "pause" | "recover" | null>(null);
  const report = status.report?.revision === status.revision ? status.report : null;
  const canChange = machineRunning && status.available && status.running && busy == null;
  const canVerify = canChange && !status.paused && status.gate !== "checking";
  const checking = busy === "verify" || status.gate === "checking";

  async function run(action: "verify" | "pause" | "recover") {
    setBusy(action);
    if (action === "verify") await attempt(networkIpc.verify());
    else if (action === "pause") await attempt(networkIpc.setPaused(!status.paused));
    else await attempt(networkIpc.setAutoRecover(!status.autoRecover));
    setBusy(null);
  }

  const when = summary.tone === "ok" && status.expiresAt != null
    ? `Verified until ${time.format(status.expiresAt)}`
    : status.nextRetryAt != null && status.autoRecover && !status.paused && summary.tone !== "ok"
      ? `Next try ${time.format(status.nextRetryAt)}`
      : report ? `Checked ${time.format(report.finishedAt)}` : null;

  return (
    <section className="grid gap-3.5" aria-label="Network check">
      <div className={`glass flex items-center gap-3.5 rounded-2xl p-3.5 ${summary.tone === "bad" ? "border-bad/30" : summary.tone === "warn" ? "border-warn/30" : summary.tone === "ok" ? "border-ok/30" : ""}`}>
        <AccessMark tone={checking ? "busy" : summary.tone} size={22} />
        <div className="grid min-w-0 flex-1 gap-px" role="status">
          <strong className="text-lg">{checking && summary.tone !== "ok" ? "Checking…" : summary.title}</strong>
          <span className="text-sm text-muted [overflow-wrap:anywhere]">{summary.detail}</span>
          {when && <small className="text-xs text-faint">{when}</small>}
        </div>
        {!loadError && !status.autoRecover && status.gate === "closed" && !status.paused && (
          <Button size="xs" disabled={!canVerify} onClick={() => void run("verify")}>Verify internet access</Button>
        )}
      </div>

      {machineRunning && status.error && <RecoveryPanel compact message={`The network gateway is not running. ${status.error}`} />}

      {report ? (
        <NetworkCheckReportView key={report.id} report={report} onFix={onRemediate} />
      ) : (
        <div className="px-3 py-6 text-center text-sm text-faint">
          {!machineRunning ? "Start the machine to check internet access." : checking ? "Running the full check…" : "No check for this route yet."}
        </div>
      )}

      <div className="flex flex-wrap gap-5 pt-1">
        <Switch size="xs" label="Pause internet" checked={status.paused} disabled={!canChange} onChange={() => void run("pause")} />
        <Switch size="xs" label="Retry on its own" checked={status.autoRecover} disabled={!canChange} onChange={() => void run("recover")} />
      </div>
    </section>
  );
}
