import { Button, NumberInput, Slider, Switch, TextInput } from "@mantine/core";
import { IconArrowRight, IconCheck, IconDeviceDesktop, IconUser } from "@tabler/icons-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import type { HostFacts, Readiness } from "../../services/ipc/generated/common";
import type { InstallPlan, MachineInstallOptions, MirrorResult, OsChoice } from "../../services/ipc/generated/core";
import { machineIpc } from "../../services/ipc/app";
import { MirrorPicker } from "../../components/MirrorPicker";
import { Note } from "../../components/Boot";
import { MachineStage } from "../../components/MachineStage";
import { DisclosureChevron, Reveal } from "../../components/motion";
import { ReadinessPanel } from "./ReadinessPanel";
import { gb, MachineCard, type MachineSpec } from "./MachineCard";
import { explainFailure } from "../../lib/wizard";
import {
  chosenOs, diskGb, formProblem, hostnameProblem, matchingPreset, mirrorHost,
  presetFitsHost, readinessView, RESOURCE_PRESETS, type ResourcePreset, usernameProblem,
} from "../../lib/installForm";

/** The working mirror with the best measured speed. */
function fastest(results: MirrorResult[]): MirrorResult | null {
  return results.filter((r) => r.status === "ok").reduce<MirrorResult | null>(
    (best, r) => (!best || (r.speedKbps ?? 0) > (best.speedKbps ?? 0) ? r : best), null);
}

/** A titled group of the setup form. */
function Group({ title, hint, children }: { title: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <section className="iw-group">
      <header className="iw-group-head">
        <h2>{title}</h2>
        {hint && <span>{hint}</span>}
      </header>
      {children}
    </section>
  );
}

/** CPU, memory and disk as short bars (the share of this computer) with their values. */
function Footprint({ cpus, memoryMb, disk, host }: { cpus: number; memoryMb: number; disk: number; host: HostFacts | null }) {
  const rows: [number | null, string, string][] = [
    [host?.cpus ? cpus / host.cpus : null, `${cpus} CPU`, "CPUs"],
    [host?.memoryMb ? memoryMb / host.memoryMb : null, gb(memoryMb), "memory"],
    [host?.maxDiskGb ? disk / host.maxDiskGb : null, `${disk} GB`, "disk"],
  ];
  return (
    <span className="iw-footprint">
      {rows.map(([share, value, what]) => (
        <span key={what} className="contents">
          <span className="iw-footprint-bar" aria-hidden>
            <span style={{ inlineSize: `${Math.round(Math.max(0.06, Math.min(1, share ?? 0.4)) * 100)}%` }} />
          </span>
          <span className="iw-footprint-value"><span className="sr-only">{what} </span>{value}</span>
        </span>
      ))}
    </span>
  );
}

function SizeCard({ preset, selected, fits, host, onSelect }: {
  preset: ResourcePreset; selected: boolean; fits: boolean; host: HostFacts | null; onSelect: () => void;
}) {
  return (
    <button type="button" role="radio" aria-checked={selected} disabled={!fits} onClick={onSelect} className="iw-option iw-size">
      <span className="iw-option-title">
        {preset.label}
        {selected && <span className="iw-tick"><IconCheck size={11} stroke={3} /></span>}
      </span>
      <span className="iw-option-note">{fits ? preset.note : "Too large for this computer"}</span>
      <Footprint cpus={preset.cpus} memoryMb={preset.memoryMb} disk={preset.diskGb} host={host} />
    </button>
  );
}

/**
 * The installer's setup: readiness first, then the choices that matter
 * (size, release, account) laid out in the open, with the mirror and device
 * profile under More options. The machine card beside it previews the result
 * and holds Install. The fastest mirror is measured in the background and
 * used unless one is chosen.
 */
export function InstallSetup({ options, spec, set, plan, choices, readiness, host, planError, checking, onInstall, onRecheck }: {
  options: MachineInstallOptions;
  spec: MachineSpec;
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
  const [more, setMore] = useState(false);
  const [custom, setCustom] = useState(preset == null);
  const [mirrorAuto, setMirrorAuto] = useState<MirrorResult | null>(null);
  const mirrorChosen = useRef(false);
  const problem = formProblem(options);
  const planHelp = planError ? explainFailure(planError) : null;
  const view = readiness ? readinessView(readiness) : null;
  const blockers = view?.blocking.length ?? 0;
  const blocked = checking || readiness == null || blockers > 0;
  const unsupported = readiness?.some((item) => item.id === "supportedHost" || item.id === "appLocation");
  const canMeasure = host != null && !unsupported;
  const user = options.username ?? "";
  const hostname = options.hostname ?? "";
  const customOpen = custom || preset == null;

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
    ? `${mirrorHost(options.aptMirror)}${mirrorAuto && mirrorAuto.uri === options.aptMirror ? " · fastest" : ""}`
    : mirrorAuto?.default ? `${mirrorAuto.host} · fastest` : "Ubuntu's archive";
  const osChoices = choices.length ? choices : [{ id: "ubuntu-24.04", name: "Ubuntu 24.04 LTS", testing: false, mirrors: [] }];

  // Sliders stop at what this computer can give (leaving 2 GB for itself).
  const cpuMax = Math.max(1, Math.min(64, host?.cpus || 16));
  const memMax = host?.memoryMb ? Math.max(1024, Math.floor((host.memoryMb - 2048) / 512) * 512) : 16384;
  const diskMax = Math.max(8, Math.min(1024, host?.maxDiskGb || 1024));

  if (unsupported) {
    return (
      <MachineStage eyebrow="Cabin setup" title="Cabin can't install a machine here yet"
        main={<ReadinessPanel items={readiness} host={host} checking={checking} onChanged={onRecheck} />} />
    );
  }

  const ready = !blocked && problem == null && plan != null && planError == null;
  const cardBadge = readiness == null || checking ? "Checking…" : blockers > 0 ? "Attention" : undefined;

  const size = [`${options.cpus ?? 2} CPU`, gb(options.memoryMb ?? 2048), `${diskGb(options.diskSize) ?? 20} GB disk`].join(" · ");

  return (
    <MachineStage
      eyebrow="Welcome to Cabin"
      title="Set up your agent machine"
      lead="Agents work inside a private Ubuntu machine on this computer. These settings were picked for it. Change anything you like."
      main={
        <>
          <ReadinessPanel items={readiness} host={host} checking={checking} onChanged={onRecheck} />
          <Group title="Size" hint="You can change it later in Machine settings">
            <div role="radiogroup" aria-label="Machine size" className="iw-options iw-options-size">
              {RESOURCE_PRESETS.map((p) => (
                <SizeCard key={p.key} preset={p} host={host} fits={presetFitsHost(p, host)} selected={!customOpen && preset?.key === p.key}
                  onSelect={() => { setCustom(false); set({ cpus: p.cpus, memoryMb: p.memoryMb, diskSize: `${p.diskGb}G` }); }} />
              ))}
              <button type="button" role="radio" aria-checked={customOpen} onClick={() => setCustom(true)} className="iw-option iw-size">
                <span className="iw-option-title">
                  Custom
                  {customOpen && <span className="iw-tick"><IconCheck size={11} stroke={3} /></span>}
                </span>
                <span className="iw-option-note">Set each value</span>
                <Footprint cpus={options.cpus ?? 2} memoryMb={options.memoryMb ?? 2048} disk={diskGb(options.diskSize) ?? 20} host={host} />
              </button>
            </div>
            <Reveal open={customOpen}>
              <div className="iw-custom">
                <label className="iw-slider">
                  <span>CPUs</span>
                  <Slider min={1} max={cpuMax} step={1} value={Math.min(options.cpus ?? 2, cpuMax)} onChange={(cpus) => set({ cpus })} label={(v) => `${v} CPU`} />
                  <strong>{options.cpus}</strong>
                </label>
                <label className="iw-slider">
                  <span>Memory</span>
                  <Slider min={1024} max={memMax} step={512} value={Math.min(options.memoryMb ?? 2048, memMax)} onChange={(memoryMb) => set({ memoryMb })} label={gb} />
                  <strong>{gb(options.memoryMb ?? 2048)}</strong>
                </label>
                <label className="iw-slider">
                  <span>Disk</span>
                  <NumberInput min={8} max={diskMax} value={diskGb(options.diskSize) ?? 20} suffix=" GB" onChange={(v) => set({ diskSize: `${Number(v) || 8}G` })} />
                  <span className="text-xs text-faint">grows as used</span>
                </label>
              </div>
            </Reveal>
            {(options.memoryMb ?? 0) < 2048 && <Note tone="warn">{gb(options.memoryMb ?? 0)} installs, but agents need about 2 GB to work comfortably.</Note>}
          </Group>

          {osChoices.length > 1 && (
            <Group title="Release">
              <div role="radiogroup" aria-label="Ubuntu release" className="iw-options iw-options-os">
                {osChoices.map((choice, index) => {
                  const selected = (os?.id ?? osChoices[0].id) === choice.id;
                  return (
                    <button key={choice.id} type="button" role="radio" aria-checked={selected} className="iw-option"
                      onClick={() => { mirrorChosen.current = false; set({ os: choice.id, aptMirror: null }); }}>
                      <span className="iw-option-title">
                        {choice.name}
                        {selected && <span className="iw-tick"><IconCheck size={11} stroke={3} /></span>}
                      </span>
                      <span className="iw-option-note">{choice.testing ? "Development release" : index === 0 ? "Recommended · supported to 2029" : "Newest LTS"}</span>
                    </button>
                  );
                })}
              </div>
            </Group>
          )}

          <Group title="Account" hint="Agents run as this user, with passwordless sudo">
            <div className="iw-fields">
              <TextInput
                label="Username"
                placeholder={plan?.username ?? "dev"}
                leftSection={<IconUser size={16} />}
                value={user}
                error={usernameProblem(user)}
                onChange={(e) => set({ username: e.currentTarget.value.trim() || null })}
              />
              <TextInput
                label="Hostname"
                placeholder={plan?.hostname ?? "dev-vm"}
                leftSection={<IconDeviceDesktop size={16} />}
                value={hostname}
                error={hostnameProblem(hostname)}
                onChange={(e) => set({ hostname: e.currentTarget.value.trim().toLowerCase() || null })}
              />
            </div>
          </Group>

          <section className="iw-group">
            <button type="button" className="iw-more" aria-expanded={more} onClick={() => setMore((v) => !v)}>
              <DisclosureChevron open={more} />
              <span>More options</span>
              <span className="iw-more-hint">Mirror: {mirrorLine}{options.device ? " · Device profile on" : ""}</span>
            </button>
            <Reveal open={more}>
              <div className="iw-more-body">
                <div>
                  <h3 className="iw-sub">Package mirror</h3>
                  <MirrorPicker key={os?.id} os={os?.id ?? null} heading={false} autoPick={false} value={options.aptMirror ?? null}
                    onChange={(aptMirror) => { mirrorChosen.current = true; set({ aptMirror }); }} />
                </div>
                {host?.deviceProfileSupported !== false && (
                  <Switch
                    label="Device profile"
                    description="Applies a hardware profile and verifies the runtime devices after setup."
                    checked={options.device}
                    onChange={(e) => set({ device: e.currentTarget.checked })}
                  />
                )}
              </div>
            </Reveal>
          </section>
        </>
      }
      aside={<MachineCard tone="draft" host={host} badge={cardBadge} caption={preset ? `${preset.label} size` : "Custom size"} spec={spec} />}
      bar={
        <>
          <div className="min-w-0 flex-1" role={problem || planHelp ? "alert" : undefined}>
            {problem || planHelp ? (
              <span className="text-sm text-bad [overflow-wrap:anywhere]">
                {problem ?? <><strong>{planHelp?.title}</strong> · {planHelp?.advice} <span className="text-faint">{planError}</span></>}
              </span>
            ) : (
              <span className="grid">
                <strong className="truncate text-md font-semibold">{spec.osName} · {size}</strong>
                <span className="truncate text-xs text-faint">{blockers > 0 ? "Resolve the computer checks to continue." : "Downloads Ubuntu and its packages. You can keep working while it installs."}</span>
              </span>
            )}
          </div>
          <Button size="md" className="shrink-0" rightSection={<IconArrowRight size={17} />} disabled={!ready} onClick={onInstall}>Install Ubuntu</Button>
        </>
      }
    />
  );
}
