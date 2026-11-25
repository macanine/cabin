import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { Button, Checkbox, Progress } from "@mantine/core";
import { IconArrowLeft, IconPlayerPlay, IconRotateClockwise } from "@tabler/icons-react";
import { useMemo, useState } from "react";
import type { DownloadProgress, InstallStep, MachineInstallOptions, MachineStatus } from "../../services/ipc/generated/core";
import { machineIpc } from "../../services/ipc/app";
import { toIpcError } from "../../services/ipc/common";
import { confirmAction } from "../../services/ipc/dialogs";
import { patchState, useApp } from "../../state/store";
import { uiActions } from "../../state/ui";
import { MirrorPicker } from "../../components/MirrorPicker";
import { type StateTone } from "../../components/common";
import { StepProgress } from "../../components/StepProgress";
import { CopyDiagnosticsButton } from "../../components/Recovery";
import { MachineStage } from "../../components/MachineStage";
import { BootDetails, BootStep, Note, Ticker } from "../../components/Boot";
import { useAutoOpen } from "../../components/motion";
import { Celebration } from "../../components/Celebration";
import type { HostFacts } from "../../services/ipc/generated/common";
import { type CardTone, MachineCard, type MachineSpec } from "./MachineCard";
import { downloadSummary, formatDuration } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import {
  currentWizardStep, explainFailure, overallProgress, stageTicker, WIZARD_STEPS, wizardState, type WizardState,
} from "../../lib/wizard";

const TONE: Record<WizardState, StateTone> = {
  pending: "pending", running: "running", done: "ok", cached: "ok", failed: "bad", rolledBack: "undone",
};

const STATE_TEXT: Record<string, string> = {
  pending: "Waiting",
  running: "In progress",
  done: "Done",
  cached: "Already prepared",
  failed: "Failed",
  rolledBack: "Rolled back",
  satisfied: "Satisfied",
  needed: "Needs applying",
};

/** Pipeline step state → wizard state (adds the "Verify" states). */
const asWizard = (state: string): WizardState =>
  state === "satisfied" ? "done" : state === "needed" ? "pending" : (state as WizardState);

const FINISHED = new Set(["done", "cached", "satisfied", "rolledBack"]);
const seconds = (steps: InstallStep[]) => steps.reduce((total, s) => total + (s.seconds ?? 0), 0);

function DownloadBar({ download }: { download: DownloadProgress }) {
  const { percent, text } = downloadSummary(download);
  return (
    <div className="mt-1.5 grid gap-1">
      <Progress
        value={percent ?? 100}
        size="sm"
        radius="xl"
        striped={percent == null || download.verifying}
        animated={percent == null || download.verifying}
        transitionDuration={260}
        color={download.verifying ? "teal" : "var(--accent)"}
        aria-label="Download progress"
      />
      <span className="text-xs text-faint">{download.verifying ? "Verifying the checksum…" : text}</span>
    </div>
  );
}

/**
 * Show overall progress and the current stage. Keep full logs under Details
 * and let users retry failures or resume an incomplete installation.
 */
export function InstallProgress({
  machine, steps, startedAt, error, installOptions, cancelled, spec, host, onRetry, onEdit,
}: {
  machine: MachineStatus;
  spec: MachineSpec;
  host: HostFacts | null;
  steps: InstallStep[];
  startedAt: number | null;
  error: string | null;
  installOptions: Pick<MachineInstallOptions, "diskSize" | "memoryMb" | "username">;
  cancelled: string | null;
  onRetry: () => void;
  onEdit: () => void;
}) {
  const [notice, setNotice] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [changeMirror, setChangeMirror] = useState(false);
  const [mirror, setMirror] = useState<string | null>(null);

  const installing = ["Installing the machine", "Cancelling the install"].includes(machine.busy ?? "");
  const installed = machine.state !== "notInstalled";
  const connected = useApp((state) => state.guestReady);
  const noAgents = useApp((state) => state.agents.every((agent) => !agent.installed));
  // Celebrate the install that just ran here, not every visit to the record.
  const justInstalled = connected && startedAt != null && !installing;
  const now = useNow(installing);
  const states = useMemo(() => WIZARD_STEPS.map((def) => wizardState(def, steps, machine)), [steps, machine]);
  const current = currentWizardStep(steps, machine);
  const failedStep = steps.find((step) => step.state === "failed");
  const failure = error ?? actionError ?? (startedAt != null && !connected ? machine.error : null);
  const failed = cancelled == null && !installing && (failure != null || failedStep != null || states.includes("failed"));
  const [details, toggleDetails] = useAutoOpen(failed);
  const help = failed ? explainFailure(failure ?? failedStep?.detail ?? "", failedStep?.id, failedStep?.logTail) : null;
  const fractions = WIZARD_STEPS.map((def) => {
    const d = steps.find((s) => def.pipeline.includes(s.id) && s.download)?.download;
    const percent = d ? downloadSummary(d).percent : null;
    return percent == null ? null : percent / 100;
  });
  const progress = justInstalled ? 1 : overallProgress(states, fractions);
  const provisioned = steps.some((s) => s.id === "provision" && ["done", "cached", "satisfied"].includes(s.state));
  const elapsed = installing && startedAt != null ? formatDuration(now - startedAt) : seconds(steps) > 0 ? formatDuration(seconds(steps) * 1000) : null;

  async function act(task: () => Promise<void>) {
    setActionError(null);
    try { await task(); } catch (err) { setActionError(toIpcError(err).message); }
  }

  useAutoRefresh(async (isCurrent) => {
    try {
      const result = await machineIpc.verify();
      if (!isCurrent()) return;
      patchState((state) => ({ ...state, install: result }));
      const missing = result.filter((s) => s.state === "needed").length;
      setNotice(missing === 0 ? "All installation steps are in place." : `${missing} installation steps need applying.`);
    } catch (error) {
      if (isCurrent()) setNotice(toIpcError(error).message);
    }
  }, { enabled: installed && !justInstalled && !machine.busy && machine.state !== "running", minGap: 30_000 });

  const resume = () => act(async () => {
    setNotice(null);
    await machineIpc.resume(changeMirror, changeMirror ? mirror : null);
  });

  const cancel = () => act(async () => {
    if (await confirmAction("Cancel the install? The machine is stopped and what was created so far is removed. The downloaded image is kept.")) {
      await machineIpc.cancelInstall();
    }
  });

  // The pipeline finished and the machine is starting for the first time.
  const starting = startedAt != null && installed && !installing && !connected && !failed && machine.state === "running";
  const title = justInstalled ? "Your machine is ready"
    : starting ? "Starting your machine"
    : installing ? machine.busy === "Cancelling the install" ? "Cancelling…" : `Installing ${spec.osName}`
      : cancelled ? "Install cancelled"
        : failed ? "The install stopped"
          : installed ? "Installation" : "Install";
  const subtitle = justInstalled
    ? `${spec.osName} is running and Cabin ${machine.guestVersion ?? ""} is connected.`
    : starting ? "Almost done: connecting to Cabin's service in the machine."
    : installing ? WIZARD_STEPS[current].title
      : cancelled ?? (failed ? help?.title : installed ? "Every step is recorded and can be verified and resumed." : undefined);

  const stepKind = (state: WizardState) => (state === "cached" ? "done" : state === "rolledBack" ? "pending" : state);
  // The card's screen narrates the running stage: the download, or its newest log line.
  const runningDownload = steps.find((s) => s.state === "running" && s.download)?.download;
  const screenLine = justInstalled ? "cabind connected · ready for agents"
    : failed ? `stopped: ${(failedStep?.title ?? WIZARD_STEPS[current].title).toLowerCase()}`
    : installing || starting ? (runningDownload ? `${runningDownload.verifying ? "verifying" : "downloading"} ${downloadSummary(runningDownload).text}` : stageTicker(steps, WIZARD_STEPS[current]) ?? `${WIZARD_STEPS[current].title.toLowerCase()}…`)
      : null;
  const leaving = installing && machine.busy === "Installing the machine";
  const retryable = help != null || (cancelled != null && !installed);
  const bar = justInstalled ? (
    <>
      <span className="min-w-0 flex-1 text-sm text-muted">{noAgents ? "Next, choose the agents to install." : "Everything is in place."}</span>
      {noAgents
        ? <Button size="md" onClick={() => uiActions.navigate({ kind: "agents" })}>Install your agents</Button>
        : <Button size="md" leftSection={<IconPlayerPlay size={16} />} onClick={() => uiActions.navigate({ kind: "new" })}>Start a task</Button>}
    </>
  ) : leaving ? (
    <>
      <span className="min-w-0 flex-1 text-sm text-muted">You can leave this page; the install continues.</span>
      <Button variant="subtle" color="red" onClick={cancel}>Cancel install</Button>
    </>
  ) : retryable ? (
    <>
      {help && <CopyDiagnosticsButton installError={failure} installOptions={installed ? undefined : installOptions} />}
      <span className="flex-1" />
      {!installed && <>
        <Button variant="default" leftSection={<IconArrowLeft size={14} />} onClick={onEdit}>Change settings</Button>
        <Button leftSection={<IconRotateClockwise size={14} />} onClick={onRetry}>{cancelled ? "Install again" : "Retry"}</Button>
      </>}
    </>
  ) : undefined;
  const showMeter = installing || starting || failed || justInstalled;
  const tone: CardTone = justInstalled ? "ready" : failed ? "failed" : installing || starting ? "building" : installed ? "idle" : "draft";

  return (
    <MachineStage
      eyebrow={installed && !installing && !justInstalled && !starting ? "Machine settings" : "Cabin setup"}
      title={title}
      lead={subtitle}
      lede={justInstalled ? <Celebration size={44} /> : undefined}
      main={
        <>
          {showMeter && (
            <div className="iw-progress" data-tone={tone}>
              <div className="iw-progress-head">
                <span className="iw-percent">{Math.round(progress * 100)}<small>%</small></span>
                <span className="iw-progress-meta">
                  <span>Installation progress</span>
                  {elapsed && <span className="tabular-nums">{elapsed}</span>}
                </span>
              </div>
              <div className="iw-segments" role="progressbar" aria-label="Installation progress" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(progress * 100)}>
                {WIZARD_STEPS.map((def, index) => {
                  const state = justInstalled ? "done" : states[index];
                  const fill = state === "done" || state === "cached" ? 1 : state === "running" ? fractions[index] : state === "failed" ? 1 : 0;
                  return (
                    <span key={def.key} data-state={state} data-indeterminate={state === "running" && fill == null} title={def.title}>
                      <span style={{ inlineSize: `${Math.round((fill ?? 0) * 100)}%` }} />
                    </span>
                  );
                })}
              </div>
            </div>
          )}

          {help && (
            <div className="iw-failure" role="alert">
              <strong>{help.title}</strong>
              <p>{help.advice}</p>
              {!installed && <p className="text-faint">Retry reuses a verified image if it was already downloaded.</p>}
              {failure && <code>{failure}</code>}
            </div>
          )}
          <ol className="iw-stages" aria-label="Installation stages">
            {WIZARD_STEPS.map((def, index) => {
              const state = justInstalled ? "done" : states[index];
              const own = steps.filter((s) => def.pipeline.includes(s.id));
              const secs = seconds(own);
              const download = own.find((s) => s.download)?.download;
              const live = state === "running";
              const ticker = live && !download ? stageTicker(steps, def) : null;
              return (
                <BootStep key={def.key} order={index} state={stepKind(state)} title={def.title} time={state === "cached" ? "cached" : secs > 0 ? formatDuration(secs * 1000) : ""}>
                  {(live || state === "failed") && <div className="text-sm text-muted">{def.description}</div>}
                  {live && download && <DownloadBar download={download} />}
                  {ticker && <Ticker title={ticker}>{ticker}</Ticker>}
                  {live && def.key === "agentd" && machine.state === "running" && !connected && (
                    <Ticker>{machine.connection === "updating" ? "Updating Cabin's service in the machine…" : machine.error ?? "Waiting for SSH…"}</Ticker>
                  )}
                </BootStep>
              );
            })}
          </ol>

          {installed && !installing && !justInstalled && !starting && (
            <section className="glass grid gap-2 rounded-xl p-3.5">
              <div className="flex flex-wrap items-center gap-2">
                <span className="min-w-0 flex-1 text-sm text-faint">Resume repairs missing steps while the machine is stopped.</span>
                <Button size="xs" onClick={resume} disabled={machine.busy != null || machine.state === "running"}>Resume</Button>
              </div>
              {notice && <div className="animate-fade text-sm text-muted">{notice}</div>}
              {/* The mirror lives in the cloud-init seed: it only matters until first boot finishes. */}
              {!provisioned && <Checkbox size="xs" label="Use another package mirror when resuming" checked={changeMirror} onChange={(e) => setChangeMirror(e.currentTarget.checked)} />}
              {changeMirror && !provisioned && <MirrorPicker os={machine.os} value={mirror} onChange={setMirror} />}
            </section>
          )}
          {actionError && !help && <Note tone="bad">{actionError}</Note>}

          {steps.length > 0 && (
            <BootDetails open={details} onToggle={toggleDetails}>
              <StepProgress
                steps={steps.map((step) => ({
                  key: step.id,
                  title: step.title,
                  tone: TONE[asWizard(step.state)],
                  status: `${STATE_TEXT[step.state] ?? step.state}${step.seconds != null && step.state !== "cached" ? ` · ${formatDuration(step.seconds * 1000)}` : ""}`,
                  finished: FINISHED.has(step.state),
                  log: step.logTail,
                  detail: step.detail,
                  logPath: step.state === "failed" ? step.logPath : null,
                }))}
              />
            </BootDetails>
          )}
        </>
      }
      aside={<MachineCard spec={spec} host={host} tone={tone} screen={screenLine}
        caption={justInstalled && machine.guestVersion ? `Cabin ${machine.guestVersion} connected` : undefined} />}
      bar={bar}
    />
  );
}
