import { Button, NumberInput, Slider, Switch, TextInput } from "@mantine/core";
import {
  IconAlertTriangle, IconArrowLeft, IconArrowRight, IconBrandUbuntu, IconCheck, IconCpu, IconDeviceDesktop, IconDownload, IconUser,
} from "@tabler/icons-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import type { HostCheck, InstallPlan, MachineInstallOptions, MirrorResult, OsChoice } from "../../services/ipc/generated/core";
import { machineIpc } from "../../services/ipc/app";
import { MirrorPicker } from "../../components/MirrorPicker";
import { Note } from "../../components/Boot";
import { Reveal } from "../../components/motion";
import { Stepper } from "../../components/Stepper";
import { uiActions } from "../../state/ui";
import {
  chosenOs, diskGb, formProblem, hostnameProblem, installSummary, isOptionalCheck, matchingPreset, mirrorHost,
  RESOURCE_PRESETS, usernameProblem,
} from "../../lib/installForm";

const gb = (mb: number) => `${mb / 1024 % 1 === 0 ? mb / 1024 : (mb / 1024).toFixed(1)} GB`;
const STEPS = ["System", "Account", "Review"] as const;

/** The working mirror with the best measured speed. */
function fastest(results: MirrorResult[]): MirrorResult | null {
  return results.filter((r) => r.status === "ok").reduce<MirrorResult | null>(
    (best, r) => (!best || (r.speedKbps ?? 0) > (best.speedKbps ?? 0) ? r : best), null);
}

/** A radio card: the chosen one is ringed in the accent and shows a check. */
function Choice({ selected, onSelect, children }: { selected: boolean; onSelect: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      onClick={onSelect}
      className={`press relative grid min-w-0 cursor-pointer content-start gap-0.5 rounded-xl border p-3.5 text-left text-fg ${
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

function HostReadiness({ checks, plan }: { checks: HostCheck[] | null; plan: InstallPlan | null }) {
  const missing = (checks ?? []).filter((c) => !isOptionalCheck(c.id) && !c.ok);
  const emulated = plan?.accel.startsWith("tcg") ?? false;
  if (checks == null) return <Note>Checking host requirements…</Note>;
  if (missing.length) {
    return (
      <Note tone="bad" role="alert">
        <div className="flex gap-2"><IconAlertTriangle size={15} className="mt-0.5 shrink-0" />
          <div className="grid gap-0.5"><strong>Host requirements missing</strong>
            {missing.map((c) => <span key={c.id} className="text-fg">{c.label}: <span className="font-mono text-xs">{c.detail}</span></span>)}
          </div>
        </div>
      </Note>
    );
  }
  return (
    <Note tone={emulated ? "warn" : "ok"}>
      <span className="flex items-center gap-2">
        {emulated ? <IconAlertTriangle size={15} /> : <IconCheck size={15} />}
        {emulated ? "QEMU is using software emulation. Installation may take longer." : "This computer is ready. Installing usually takes a few minutes."}
      </span>
    </Note>
  );
}

/**
 * The installer's setup as a three-step wizard: system and size, the
 * account and package mirror, then a review with Install. The fastest mirror
 * is measured in the background and used unless one is chosen.
 */
export function InstallSetup({ options, set, plan, choices, checks, planError, onInstall }: {
  options: MachineInstallOptions;
  set: (patch: Partial<MachineInstallOptions>) => void;
  plan: InstallPlan | null;
  choices: OsChoice[];
  checks: HostCheck[] | null;
  planError: string | null;
  onInstall: () => void;
}) {
  const os = chosenOs(options, choices);
  const preset = matchingPreset(options);
  const [step, setStep] = useState(0);
  const [custom, setCustom] = useState(preset == null);
  const [mirrorAuto, setMirrorAuto] = useState<MirrorResult | null>(null);
  const mirrorChosen = useRef(false);
  const problem = formProblem(options);
  const blocked = (checks ?? []).some((c) => !isOptionalCheck(c.id) && !c.ok);
  const user = options.username ?? "";
  const host = options.hostname ?? "";

  // Measure the mirrors for this system once; use the fastest unless one was picked.
  useEffect(() => {
    let alive = true;
    setMirrorAuto(null);
    void machineIpc.testMirrors(os?.id ?? null, []).then((results) => {
      if (!alive) return;
      const best = fastest(results);
      setMirrorAuto(best);
      if (best && !mirrorChosen.current) set({ aptMirror: best.default ? null : best.uri });
    }).catch(() => undefined);
    return () => { alive = false; };
  }, [os?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const mirrorLine = options.aptMirror
    ? `${mirrorHost(options.aptMirror)}${mirrorAuto && mirrorAuto.uri === options.aptMirror ? " (fastest)" : ""}`
    : mirrorAuto?.default ? `${mirrorAuto.host} (fastest)` : "Ubuntu's own archive";
  const osChoices = choices.length ? choices : [{ id: "ubuntu-24.04", name: "Ubuntu 24.04 LTS", testing: false, mirrors: [] }];
  const accountProblem = usernameProblem(user) ?? hostnameProblem(host);

  return (
    <div className="grid min-w-0 gap-6">
      <header className="grid justify-items-center gap-1.5 text-center">
        <span className="glass mb-1 grid size-14 place-items-center rounded-2xl text-accent"><IconBrandUbuntu size={30} /></span>
        <h1 className="m-0 text-2xl font-semibold tracking-tight">Set up your agent machine</h1>
        <p className="m-0 max-w-[480px] text-md text-muted">Run Claude Code, Codex, OpenCode, Pi and Antigravity in one local Ubuntu VM.</p>
        <Button variant="subtle" size="compact-sm" onClick={() => uiActions.navigate({ kind: "settings", tab: "machine" })}>
          Machine settings
        </Button>
      </header>

      <Stepper steps={STEPS} step={step} onGo={setStep} />

      <section key={step} className="view-enter grid min-w-0 gap-5">
        {step === 0 && (
          <>
            <HostReadiness checks={checks} plan={plan} />
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
                  <Choice key={p.key} selected={!custom && preset?.key === p.key} onSelect={() => { setCustom(false); set({ cpus: p.cpus, memoryMb: p.memoryMb, diskSize: `${p.diskGb}G` }); }}>
                    <strong className="flex flex-wrap items-center gap-x-1.5 pr-6 text-md">{p.label}{p.key === "balanced" && <span className="rounded-full bg-accent-soft px-1.5 py-px text-[10.5px] font-medium text-accent">Best</span>}</strong>
                    <span className="font-mono text-xs">{p.cpus} CPU · {gb(p.memoryMb)} · {p.diskGb} GB</span>
                    <Sub>{p.note}</Sub>
                  </Choice>
                ))}
                <Choice selected={custom} onSelect={() => setCustom(true)}>
                  <strong className="flex items-center gap-1.5 text-md"><IconCpu size={14} /> Custom</strong>
                  <Sub>Set each value</Sub>
                </Choice>
              </div>
              <Reveal open={custom}>
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
          </>
        )}

        {step === 1 && (
          <>
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
                value={host}
                error={hostnameProblem(host)}
                onChange={(e) => set({ hostname: e.currentTarget.value.trim().toLowerCase() || null })}
              />
            </div>
            <div>
              <Heading hint={`now: ${mirrorLine}`}>Package mirror</Heading>
              <MirrorPicker key={os?.id} os={os?.id ?? null} heading={false} autoPick={false} value={options.aptMirror ?? null}
                onChange={(aptMirror) => { mirrorChosen.current = true; set({ aptMirror }); }} />
            </div>
            <div className="glass rounded-xl p-3.5">
              <Switch
                label="Device profile"
                description="Applies a hardware profile and verifies the runtime devices after setup."
                checked={options.device}
                onChange={(e) => set({ device: e.currentTarget.checked })}
              />
            </div>
          </>
        )}

        {step === 2 && (
          <>
            <dl className="glass m-0 grid gap-px overflow-hidden rounded-xl bg-line/60 text-md">
              {([
                ["System", os?.name ?? "Ubuntu 24.04 LTS"],
                ["Size", `${options.cpus} CPU · ${gb(options.memoryMb ?? 2048)} · ${diskGb(options.diskSize) ?? 20} GB disk`],
                ["Account", `${user || plan?.username || "dev"}@${host || plan?.hostname || "dev-vm"}`],
                ["Packages", mirrorLine],
                ["Device profile", options.device ? "On" : "Off"],
              ] as const).map(([label, value]) => (
                <div key={label} className="grid grid-cols-[120px_minmax(0,1fr)] gap-3 bg-surface/70 px-4 py-2.5">
                  <dt className="text-muted">{label}</dt><dd className="m-0 font-medium [overflow-wrap:anywhere]">{value}</dd>
                </div>
              ))}
            </dl>
            {plan && (
              <dl className="m-0 grid grid-cols-[90px_minmax(0,1fr)] gap-x-3 gap-y-1 px-1 text-xs text-faint">
                {([["Image", plan.imageIndex], ["QEMU", `${plan.qemuRelease}${plan.qemuInstalled ? "" : " (downloaded)"}`], ["Acceleration", plan.accel], ["Disk", plan.diskPath], ["SSH", `127.0.0.1:${plan.sshPort}`]] as const).map(([label, value]) => (
                  <div key={label} className="contents"><dt>{label}</dt><dd className="m-0 font-mono [overflow-wrap:anywhere]">{value}</dd></div>
                ))}
              </dl>
            )}
            {(problem || planError) && <Note tone="bad" role="alert">{problem ?? planError}</Note>}
          </>
        )}
      </section>

      <footer className="flex items-center justify-between gap-3">
        <Button variant="subtle" color="gray" leftSection={<IconArrowLeft size={16} />} style={{ visibility: step === 0 ? "hidden" : "visible" }} onClick={() => setStep(step - 1)}>Back</Button>
        {step < STEPS.length - 1 ? (
          <Button rightSection={<IconArrowRight size={18} />} disabled={step === 1 && accountProblem != null} onClick={() => setStep(step + 1)}>Continue</Button>
        ) : (
          <Button size="md" leftSection={<IconDownload size={18} />} disabled={problem != null || plan == null || planError != null || blocked} onClick={onInstall}>
            Install
          </Button>
        )}
      </footer>
    </div>
  );
}
