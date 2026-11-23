import { Button, NumberInput, Slider, Switch, TextInput } from "@mantine/core";
import {
  IconBrandUbuntu, IconCheck, IconCpu, IconDeviceDesktop, IconDownload, IconSettings, IconUser,
} from "@tabler/icons-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import type { HostFacts, Readiness } from "../../services/ipc/generated/common";
import type { InstallPlan, MachineInstallOptions, MirrorResult, OsChoice } from "../../services/ipc/generated/core";
import { machineIpc } from "../../services/ipc/app";
import { MirrorPicker } from "../../components/MirrorPicker";
import { Note } from "../../components/Boot";
import { DisclosureChevron, Reveal } from "../../components/motion";
import { ReadinessPanel } from "./ReadinessPanel";
import { explainFailure } from "../../lib/wizard";
import {
  chosenOs, diskGb, formProblem, hostnameProblem, installSummary, matchingPreset, mirrorHost,
  presetFitsHost, readinessView, RESOURCE_PRESETS, setupSummary, usernameProblem,
} from "../../lib/installForm";

const gb = (mb: number) => `${mb / 1024 % 1 === 0 ? mb / 1024 : (mb / 1024).toFixed(1)} GB`;

/** The working mirror with the best measured speed. */
function fastest(results: MirrorResult[]): MirrorResult | null {
  return results.filter((r) => r.status === "ok").reduce<MirrorResult | null>(
    (best, r) => (!best || (r.speedKbps ?? 0) > (best.speedKbps ?? 0) ? r : best), null);
}

/** A radio card: the chosen one is ringed in the accent and shows a check. */
function Choice({ selected, onSelect, children, disabled = false }: { selected: boolean; onSelect: () => void; children: ReactNode; disabled?: boolean }) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      disabled={disabled}
      onClick={onSelect}
      className={`press relative grid min-w-0 cursor-pointer content-start gap-0.5 rounded-xl border p-3.5 text-left text-fg disabled:cursor-default disabled:opacity-60 ${
        selected ? "border-accent bg-accent-soft shadow-[0_0_0_3px_var(--accent-soft)]" : "glass hover:border-accent/50"
      }`}
    >
      {children}
      {selected && <span className="absolute top-3 right-3 grid size-[18px] place-items-center rounded-full bg-accent text-white"><IconCheck size={12} stroke={3} /></span>}
    </button>
  );
}

const Sub = ({ children }: { children: ReactNode }) => <span className="text-sm text-muted">{children}</span>;
const Heading = ({ children, hint }: { children: ReactNode; hint?: ReactNode }) => (
  <div className="mb-2.5 flex items-baseline justify-between gap-3">
    <h2 className="m-0 text-xs font-semibold tracking-wider text-muted uppercase">{children}</h2>
    {hint && <span className="text-xs text-faint">{hint}</span>}
  </div>
);

/**
 * The installer's setup on one screen: what this computer needs, a one-line
 * summary of the defaults chosen for it, Install, and everything adjustable
 * folded under Advanced. The fastest mirror is measured in the background
 * and used unless one is chosen.
 */
export function InstallSetup({ options, set, plan, choices, readiness, host, planError, checking, onInstall, onRecheck }: {
  options: MachineInstallOptions;
  set: (patch: Partial<MachineInstallOptions>) => void;
  plan: InstallPlan | null;
  choices: OsChoice[];
  readiness: Readiness[] | null;
  host: HostFacts | null;
  planError: string | null;
  checking: boolean;
  onInstall: () => void;
  onRecheck: () => void;
}) {
  const os = chosenOs(options, choices);
  const preset = matchingPreset(options);
  const [advanced, setAdvanced] = useState(false);
  const [custom, setCustom] = useState(preset == null);
  const [mirrorAuto, setMirrorAuto] = useState<MirrorResult | null>(null);
  const mirrorChosen = useRef(false);
  const problem = formProblem(options);
  const planHelp = planError ? explainFailure(planError) : null;
  const blocked = checking || readiness == null || readinessView(readiness).blocking.length > 0;
  const unsupported = readiness?.some((item) => item.id === "supportedHost" || item.id === "appLocation");
  const canMeasure = host != null && !unsupported;
  const user = options.username ?? "";
  const host_ = options.hostname ?? "";

  // Measure the mirrors for this system once; use the fastest unless one was picked.
  useEffect(() => {
    if (!canMeasure) return;
    let alive = true;
    setMirrorAuto(null);
    void machineIpc.testMirrors(os?.id ?? null, []).then((results) => {
      if (!alive) return;
      const best = fastest(results);
      setMirrorAuto(best);
      if (best && !mirrorChosen.current) set({ aptMirror: best.default ? null : best.uri });
    }).catch(() => undefined);
    return () => { alive = false; };
  }, [canMeasure, os?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const mirrorLine = options.aptMirror
    ? `${mirrorHost(options.aptMirror)}${mirrorAuto && mirrorAuto.uri === options.aptMirror ? " (fastest)" : ""}`
    : mirrorAuto?.default ? `${mirrorAuto.host} (fastest)` : "Ubuntu's own archive";
  const osChoices = choices.length ? choices : [{ id: "ubuntu-24.04", name: "Ubuntu 24.04 LTS", testing: false, mirrors: [] }];
  const checks = <ReadinessPanel items={readiness} checking={checking} onChanged={onRecheck} />;

  if (unsupported) return (
    <div className="view-enter grid gap-4">
      <h1 className="m-0 text-2xl font-semibold tracking-tight">Cabin setup</h1>
      {checks}
    </div>
  );

  return (
    <div className="view-enter grid min-w-0 gap-6">
      <header className="grid justify-items-center gap-1.5 text-center">
        <span className="glass mb-1 grid size-14 place-items-center rounded-2xl text-accent"><IconBrandUbuntu size={30} /></span>
        <h1 className="m-0 text-2xl font-semibold tracking-tight">Set up your agent machine</h1>
        <p className="m-0 max-w-[560px] text-md text-balance text-muted">Claude Code, Codex, OpenCode, Pi and Antigravity run in one Ubuntu machine on this computer. Cabin sets it up for you.</p>
      </header>

      {checks}

      <section className="glass grid gap-3 rounded-xl p-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <span className="min-w-0 text-md font-medium [overflow-wrap:anywhere]">{setupSummary(options, os, plan?.username ?? "dev")}</span>
          <Button size="md" leftSection={<IconDownload size={18} />} disabled={problem != null || plan == null || planError != null || blocked} onClick={onInstall}>
            Install
          </Button>
        </div>
        {(problem || planHelp) && <Note tone="bad" role="alert">
          {problem ?? <><strong>{planHelp?.title}</strong> · {planHelp?.advice}<br /><span className="text-faint">{planError}</span></>}
        </Note>}
      </section>

      <div>
        <button
          type="button"
          aria-expanded={advanced}
          onClick={() => setAdvanced((v) => !v)}
          className="press flex cursor-pointer items-center gap-2 border-0 bg-transparent px-1 py-1 text-sm font-medium text-muted hover:text-fg"
        >
          <IconSettings size={15} /> Advanced <DisclosureChevron open={advanced} />
        </button>
        <Reveal open={advanced}>
          <div className="grid min-w-0 gap-6 px-1 pt-4 pb-1">
            <div>
              <Heading>Ubuntu</Heading>
              <div role="radiogroup" aria-label="Ubuntu release" className="grid grid-cols-[repeat(auto-fit,minmax(200px,1fr))] gap-2.5">
                {osChoices.map((choice, index) => (
                  <Choice key={choice.id} selected={(os?.id ?? "ubuntu-24.04") === choice.id} onSelect={() => { mirrorChosen.current = false; set({ os: choice.id, aptMirror: null }); }}>
                    <strong className="text-md">{choice.name}</strong>
                    <Sub>{choice.testing ? "Development release" : index === 0 ? "Recommended · supported to 2029" : "Newest LTS"}</Sub>
                  </Choice>
                ))}
              </div>
            </div>
            <div>
              <Heading hint={installSummary(options, os)}>Size</Heading>
              <div role="radiogroup" aria-label="Machine size" className="grid grid-cols-[repeat(auto-fit,minmax(170px,1fr))] gap-2.5">
                {RESOURCE_PRESETS.map((p) => (
                  <Choice key={p.key} disabled={!presetFitsHost(p, host)} selected={!custom && preset?.key === p.key} onSelect={() => { setCustom(false); set({ cpus: p.cpus, memoryMb: p.memoryMb, diskSize: `${p.diskGb}G` }); }}>
                    <strong className="flex flex-wrap items-center gap-x-1.5 pr-6 text-md">{p.label}</strong>
                    <span className="font-mono text-xs">{p.cpus} CPU · {gb(p.memoryMb)} · {p.diskGb} GB</span>
                    <Sub>{p.note}</Sub>
                  </Choice>
                ))}
                <Choice selected={custom || preset == null} onSelect={() => setCustom(true)}>
                  <strong className="flex items-center gap-1.5 text-md"><IconCpu size={14} /> Custom</strong>
                  <Sub>Set each value</Sub>
                </Choice>
              </div>
              <Reveal open={custom || preset == null}>
                <div className="glass mt-3 grid gap-4 rounded-xl p-4">
                  {([
                    ["CPUs", <Slider key="c" min={1} max={16} step={1} value={options.cpus ?? 2} onChange={(cpus) => set({ cpus })} marks={[{ value: 1 }, { value: 4 }, { value: 8 }, { value: 16 }]} />, String(options.cpus)],
                    ["Memory", <Slider key="m" min={1024} max={16384} step={512} value={options.memoryMb ?? 2048} onChange={(memoryMb) => set({ memoryMb })} label={gb} />, gb(options.memoryMb ?? 2048)],
                  ] as const).map(([label, slider, value]) => (
                    <label key={label} className="grid grid-cols-[72px_minmax(0,1fr)_64px] items-center gap-4 text-sm">
                      <span className="text-muted">{label}</span>{slider}<strong className="text-right font-mono">{value}</strong>
                    </label>
                  ))}
                  <label className="grid grid-cols-[72px_minmax(0,1fr)_64px] items-center gap-4 text-sm">
                    <span className="text-muted">Disk</span>
                    <NumberInput min={8} max={1024} value={diskGb(options.diskSize) ?? 20} suffix=" GB" onChange={(v) => set({ diskSize: `${Number(v) || 8}G` })} />
                    <span className="text-xs text-faint">grows as used</span>
                  </label>
                </div>
              </Reveal>
              {(options.memoryMb ?? 0) < 2048 && <div className="mt-3"><Note tone="warn">{gb(options.memoryMb ?? 0)} installs, but agents need about 2 GB to work comfortably.</Note></div>}
            </div>
            <div className="grid grid-cols-2 gap-3 max-sm:grid-cols-1">
              <TextInput
                label="Username"
                description="The Linux account agents run as (passwordless sudo)."
                placeholder={plan?.username ?? "dev"}
                leftSection={<IconUser size={16} />}
                value={user}
                error={usernameProblem(user)}
                onChange={(e) => set({ username: e.currentTarget.value.trim() || null })}
              />
              <TextInput
                label="Hostname"
                description="Shown in the shell prompt and on the machine's network."
                placeholder={plan?.hostname ?? "dev-vm"}
                leftSection={<IconDeviceDesktop size={16} />}
                value={host_}
                error={hostnameProblem(host_)}
                onChange={(e) => set({ hostname: e.currentTarget.value.trim().toLowerCase() || null })}
              />
            </div>
            <div>
              <Heading hint={`now: ${mirrorLine}`}>Package mirror</Heading>
              <MirrorPicker key={os?.id} os={os?.id ?? null} heading={false} autoPick={false} value={options.aptMirror ?? null}
                onChange={(aptMirror) => { mirrorChosen.current = true; set({ aptMirror }); }} />
            </div>
            {host?.deviceProfileSupported !== false && (
              <div className="glass rounded-xl p-3.5">
                <Switch
                  label="Device profile"
                  description="Applies a hardware profile and verifies the runtime devices after setup."
                  checked={options.device}
                  onChange={(e) => set({ device: e.currentTarget.checked })}
                />
              </div>
            )}
          </div>
        </Reveal>
      </div>
    </div>
  );
}
