import { useCallback, useEffect, useState } from "react";
import type { HostFacts, Readiness } from "../services/ipc/generated/common";
import type { InstallPlan, MachineInstallOptions, OsChoice } from "../services/ipc/generated/core";
import { machineIpc } from "../services/ipc/app";
import { toIpcError } from "../services/ipc/common";
import { useApp } from "../state/store";
import { chosenOs, defaultOptions, diskGb, formProblem, optionsForHost } from "../lib/installForm";
import { InstallSetup } from "./install/InstallSetup";
import { InstallProgress } from "./install/InstallProgress";
import { BootPage } from "../components/Boot";

// The draft survives leaving the page (e.g. to check Environment) until the
// install starts or the app restarts.
let draft: MachineInstallOptions | null = null;
// Host-picked defaults apply once per draft; later edits are the user's.
let hostApplied = false;

/**
 * The machine installer: one setup screen (readiness, a summary of the
 * defaults picked for this computer, Install, advanced folded away), then the
 * install as a live progress view. An installed
 * machine opens on that view with automatic verification and a Resume action.
 */
export function InstallWizard() {
  const machine = useApp((state) => state.machine);
  const steps = useApp((state) => state.install);
  const [options, setOptions] = useState<MachineInstallOptions>(() => draft ?? defaultOptions());
  const [plan, setPlan] = useState<InstallPlan | null>(null);
  const [choices, setChoices] = useState<OsChoice[]>([]);
  const [planError, setPlanError] = useState<string | null>(null);
  const [readiness, setReadiness] = useState<Readiness[] | null>(null);
  const [host, setHost] = useState<HostFacts | null>(null);
  const [editing, setEditing] = useState(false);
  const [startedAt, setStartedAt] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [cancelled, setCancelled] = useState<string | null>(null);

  const installing = machine?.busy != null && /install/i.test(machine.busy);
  const installed = machine != null && machine.state !== "notInstalled";
  const showProgress = installing || installed || (!editing && (steps.length > 0 || error != null || cancelled != null));

  useEffect(() => { draft = options; }, [options]);

  // Readiness follows the chosen size. The first answer also picks the
  // size preset and user name for this computer, unless the user already did.
  const disk = diskGb(options.diskSize) ?? 20;
  const memory = options.memoryMb ?? 4096;
  const [recheck, setRecheck] = useState(0);
  useEffect(() => {
    if (showProgress) return;
    let alive = true;
    const timer = setTimeout(() => {
      machineIpc.checks(disk, memory).then((report) => {
        if (!alive) return;
        setReadiness(report.items);
        setHost(report.host);
        if (!hostApplied) {
          hostApplied = true;
          if (draft == null || JSON.stringify(draft) === JSON.stringify(defaultOptions())) {
            setOptions((value) => ({ ...value, ...optionsForHost(report.host), os: value.os, aptMirror: value.aptMirror }));
          }
        }
      }).catch(() => { if (alive) setReadiness([]); });
    }, 250);
    return () => { alive = false; clearTimeout(timer); };
  }, [disk, memory, recheck, showProgress]);
  const onRecheck = useCallback(() => setRecheck((n) => n + 1), []);

  // The plan (defaults, paths, acceleration) follows the options.
  useEffect(() => {
    if (showProgress) return;
    const timer = setTimeout(() => {
      machineIpc.plan(options)
        .then((next) => { setPlan(next); setChoices(next.osChoices); setPlanError(null); })
        .catch((err: unknown) => setPlanError(toIpcError(err).message));
    }, 250);
    return () => clearTimeout(timer);
  }, [options, showProgress]);

  async function install() {
    if (formProblem(options)) { setEditing(true); return; }
    setEditing(false);
    setError(null);
    setCancelled(null);
    setStartedAt(Date.now());
    try {
      await machineIpc.install(options);
      draft = null;
    } catch (err) {
      const ipc = toIpcError(err);
      if (ipc.code === "cancelled") setCancelled(ipc.message);
      else setError(ipc.message);
    }
  }

  if (!machine) return <div className="page" />;

  return (
    <BootPage wide={!showProgress}>
      {showProgress ? (
        <InstallProgress
          machine={machine}
          steps={steps}
          startedAt={startedAt}
          error={error}
          cancelled={cancelled}
          osName={chosenOs(options, choices)?.name ?? plan?.osName ?? "Ubuntu"}
          onRetry={install}
          onEdit={() => setEditing(true)}
        />
      ) : (
        <InstallSetup
          options={options}
          set={(patch) => setOptions((value) => ({ ...value, ...patch }))}
          plan={plan}
          choices={choices}
          readiness={readiness}
          host={host}
          onRecheck={onRecheck}
          planError={planError}
          onInstall={() => void install()}
        />
      )}
    </BootPage>
  );
}
