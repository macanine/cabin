import { useAutoRefresh } from "../lib/useAutoRefresh";
import { Button, Checkbox, Loader, Modal, Radio, Select, Switch, TextInput } from "@mantine/core";
import { IconArrowLeft, IconArrowRight, IconDownload } from "@tabler/icons-react";
import { useDebouncedValue } from "@mantine/hooks";
import { useEffect, useMemo, useState } from "react";
import type { NodeRelease, NodeState, OperationRecord } from "../services/ipc/generated/core";
import { nodeIpc } from "../services/ipc/node";
import { toIpcError } from "../services/ipc/common";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";
import {
  archiveUrl,
  LINE_STATUS_LABEL,
  NODE_MIRRORS,
  recommendedRelease,
  releaseLines,
  type LineStatus,
} from "../lib/nodeVersions";
import { OperationDetail } from "./OperationCenter";
import { guestNow } from "../lib/clock";
import { Stepper } from "./Stepper";
import { Note } from "./Boot";

const STEPS = ["Source", "Version", "Review", "Install"] as const;
type Step = 0 | 1 | 2 | 3;

const STATUS_TONE: Record<LineStatus, string> = {
  activeLts: "ok",
  maintenanceLts: "info",
  current: "warn",
  endOfLife: "bad",
};

/**
 * Installs and switches Node.js inside the machine: choose a download source,
 * pick a release line and build, review exactly what will change, then follow
 * the install live. Defaults to the Active LTS from the official builds.
 */
export function NodeVersionWizard({
  opened,
  state,
  onClose,
  onChanged,
}: {
  opened: boolean;
  state: NodeState | null;
  onClose: () => void;
  onChanged: () => void;
}) {
  const [step, setStep] = useState<Step>(0);
  const [mirrorChoice, setMirrorChoice] = useState<string>(NODE_MIRRORS[0].value);
  const [customMirror, setCustomMirror] = useState("");
  const [releases, setReleases] = useState<NodeRelease[] | null>(null);
  const [loadedMirror, setLoadedMirror] = useState<string | null>(null);
  const [failure, setFailure] = useState<{ mirror: string; message: string } | null>(null);
  const [major, setMajor] = useState<number | null>(null);
  const [version, setVersion] = useState<string | null>(null);
  const [showAll, setShowAll] = useState(false);
  const [activate, setActivate] = useState(true);
  const [startedAt, setStartedAt] = useState<number | null>(null);
  const [result, setResult] = useState<OperationRecord | null>(null);
  const [installError, setInstallError] = useState<string | null>(null);

  const mirror = mirrorChoice === "custom" ? customMirror.trim() : mirrorChoice;
  const [settledMirror] = useDebouncedValue(mirror, 650);
  const canLoad = mirror === settledMirror && /^https?:\/\/[^\s/]+/i.test(mirror);
  const loadError = failure?.mirror === mirror ? failure.message : null;
  const loading = canLoad && loadedMirror !== mirror && !loadError;
  const minimum = state?.minimumMajor ?? 22;
  const lines = useMemo(() => (releases ? releaseLines(releases, minimum) : []), [releases, minimum]);
  const visibleLines = showAll ? lines : lines.filter((line) => line.status !== "endOfLife");
  const line = lines.find((l) => l.major === major) ?? null;
  const release = line?.releases.find((r) => r.version === version) ?? null;

  // The running install, followed live from the operation event stream.
  const live = useApp((s) =>
    startedAt == null
      ? null
      : (s.operations
          .filter((op) => op.kind === "node.install" && op.createdAt >= startedAt - 1000)
          .sort((a, b) => b.createdAt - a.createdAt)[0] ?? null),
  );
  const record = result ?? live;

  // Fresh wizard on every open, starting from the saved mirror.
  useEffect(() => {
    if (!opened) return;
    const saved = state?.mirror ?? NODE_MIRRORS[0].value;
    const preset = NODE_MIRRORS.find((m) => m.value === saved);
    setMirrorChoice(preset ? preset.value : "custom");
    setCustomMirror(preset ? "" : saved);
    setStep(0);
    setReleases(null);
    setFailure(null);
    setLoadedMirror(null);
    setMajor(null);
    setVersion(null);
    setShowAll(false);
    setActivate(true);
    setStartedAt(null);
    setResult(null);
    setInstallError(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [opened]);

  useAutoRefresh(async (isCurrent) => {
    try {
      const list = await nodeIpc.releases(mirror);
      if (list.length === 0) throw new Error(`No builds for ${state?.platform ?? "this machine"} on this mirror`);
      if (!isCurrent()) return;
      setReleases(list);
      setLoadedMirror(mirror);
      setFailure(null);
      const recommended = recommendedRelease(releaseLines(list, minimum));
      setMajor(recommended?.major ?? null);
      setVersion(recommended?.version ?? null);
    } catch (error) {
      if (isCurrent()) setFailure({ mirror, message: toIpcError(error).message });
    }
  }, { enabled: opened && step === 0 && canLoad && loadedMirror !== mirror, interval: 12_000, refreshKey: mirror });

  async function install() {
    if (!release) return;
    setStep(3);
    setResult(null);
    setInstallError(null);
    setStartedAt(guestNow()); // compared with the guest's op.createdAt
    try {
      const finished = await nodeIpc.install(release.version, mirror, activate);
      setResult(finished);
      if (finished.state === "succeeded") {
        uiActions.toast(
          activate ? `Node.js ${release.version} is now the default` : `Node.js ${release.version} installed`,
          "success",
        );
      }
    } catch (error) {
      setInstallError(toIpcError(error).message);
    } finally {
      onChanged();
    }
  }

  const running = step === 3 && result == null && installError == null;
  const current = state?.installed.find((i) => i.current)?.version ?? null;
  const alreadyDefault = release != null && current === release.version;

  return (
    <Modal
      opened={opened}
      onClose={running ? () => undefined : onClose}
      closeOnClickOutside={!running}
      withCloseButton={!running}
      title="Node.js versions"
      size="lg"
      centered
    >
      <div className="mb-5"><Stepper steps={STEPS} step={step} /></div>

      {step === 0 && (
        <div key={step} className="grid animate-fade gap-3.5 text-md">
          <p className="m-0 text-muted">
            Some agent CLIs require Node.js inside the machine. Cabin installs the official {state?.platform ?? "linux-arm64"}{" "}
            builds, checks each download against its published SHA-256 sum, and can keep several versions side by
            side. Choose where to download from.
          </p>
          <Radio.Group value={mirrorChoice} onChange={setMirrorChoice}>
            <div className="grid gap-3">
              {NODE_MIRRORS.map((m) => (
                <Radio
                  key={m.value}
                  value={m.value}
                  label={m.label}
                  description={
                    <>
                      {m.hint} · <span className="font-mono text-xs">{m.value}</span>
                    </>
                  }
                />
              ))}
              <Radio value="custom" label="Custom mirror" description="Any mirror with the nodejs.org/dist layout" />
            </div>
          </Radio.Group>
          {mirrorChoice === "custom" && (
            <TextInput
              placeholder="https://mirror.example.com/node"
              value={customMirror}
              onChange={(event) => setCustomMirror(event.currentTarget.value)}
              autoFocus
            />
          )}
          <p className="m-0 text-[13px] text-faint">
            Downloads go through the proxy from Environment, if one is applied.
          </p>
          {loadError && <Note tone="bad">{loadError}</Note>}
        </div>
      )}

      {step === 1 && (
        <div key={step} className="grid animate-fade gap-3.5 text-md">
          <p className="m-0 text-muted">
            Pick a release line. <strong>Active LTS</strong> is the right choice for agents; Current lines get new
            features first but are supported for a shorter time.
          </p>
          <Radio.Group
            value={major == null ? "" : String(major)}
            onChange={(value) => {
              const next = lines.find((l) => l.major === Number(value));
              setMajor(next?.major ?? null);
              setVersion(next?.latest.version ?? null);
            }}
          >
            <div className="grid max-h-80 gap-3 overflow-y-auto py-0.5 pr-1">
              {visibleLines.map((l) => (
                <Radio
                  key={l.major}
                  value={String(l.major)}
                  label={
                    <span className="flex flex-wrap items-center gap-2">
                      <strong>Node.js {l.major}</strong>
                      {l.codename && <span className="text-faint">“{l.codename}”</span>}
                      <span className={`pill ${STATUS_TONE[l.status]}`}>{LINE_STATUS_LABEL[l.status]}</span>
                      {l.installed && <span className="pill">Installed</span>}
                      {l.major === recommendedRelease(lines)?.major && <span className="pill ok">Recommended</span>}
                    </span>
                  }
                  description={`Newest: ${l.latest.version} · ${l.latest.date}${l.latest.npm ? ` · npm ${l.latest.npm}` : ""}`}
                />
              ))}
            </div>
          </Radio.Group>
          <Checkbox
            label="Show end-of-life lines"
            checked={showAll}
            onChange={(event) => setShowAll(event.currentTarget.checked)}
          />
          {line && (
            <Select
              label="Build"
              description="The newest build has the latest security fixes"
              value={version}
              onChange={setVersion}
              allowDeselect={false}
              searchable
              data={line.releases.map((r) => ({
                value: r.version,
                label: `${r.version} · ${r.date}${r.security ? " · security release" : ""}${r.installed ? " · installed" : ""}`,
              }))}
            />
          )}
          {line?.status === "endOfLife" && (
            <Note tone="warn">Node.js {line.major} no longer gets security fixes, and agent CLIs need {minimum} or newer.</Note>
          )}
        </div>
      )}

      {step === 2 && release && (
        <div key={step} className="grid animate-fade gap-3.5 text-md">
          <div className="glass grid grid-cols-[max-content_minmax(0,1fr)] gap-x-4 gap-y-2 rounded-xl p-3.5 text-[13px] [&>div:nth-child(odd)]:text-muted [&>div:nth-child(even)]:[overflow-wrap:anywhere]">
            <div>Version</div>
            <div>
              <strong>{release.version}</strong>
              {line && <span className="text-faint"> · {LINE_STATUS_LABEL[line.status]}{line.codename ? ` “${line.codename}”` : ""}</span>}
            </div>
            <div>Download</div>
            <div className="font-mono text-xs">{release.installed ? "Already installed, nothing to download" : archiveUrl(mirror, release.version, state?.platform ?? "linux-arm64")}</div>
            <div>Verification</div>
            <div>SHA-256 against the mirror's SHASUMS256.txt</div>
            <div>Installed in</div>
            <div className="font-mono text-xs">
              {state?.installDir ?? "/opt/node"}/{release.version}
            </div>
          </div>
          <Switch
            checked={activate}
            onChange={(event) => setActivate(event.currentTarget.checked)}
            label={`Make ${release.version} the default node`}
            description={
              alreadyDefault
                ? "It already is."
                : "Sessions and ~/.local/bin/node, npm and npx switch to it. Running sessions keep their version until restarted."
            }
          />
          <ul className="m-0 grid list-disc gap-1 pl-[18px] text-[13px] text-muted">
            <li>Agent CLIs installed in ~/.local keep working; nothing is reinstalled.</li>
            {state?.system && (
              <li>
                Ubuntu's node {state.system.version} stays installed but no longer comes first on PATH.
              </li>
            )}
            {current && current !== release.version && activate && (
              <li>{current} stays installed; you can switch back at any time.</li>
            )}
            <li>If any step fails, the change is rolled back and the previous default stays.</li>
          </ul>
        </div>
      )}

      {step === 3 && (
        <div key={step} className="grid animate-fade gap-3.5 text-md">
          {installError ? (
            <Note tone="bad">{installError}</Note>
          ) : record ? (
            <OperationDetail record={record} />
          ) : (
            <div className="flex items-center gap-2.5 text-muted">
              <Loader size="sm" /> Starting…
            </div>
          )}
          {record?.state === "succeeded" && (
            <Note tone="ok">
              {activate
                ? `Done. New sessions use Node.js ${release?.version}. Restart running sessions to switch them.`
                : `Done. Node.js ${release?.version} is installed; make it the default from Setup when you need it.`}
            </Note>
          )}
        </div>
      )}

      <div className="mt-5 flex items-center gap-2.5">
        {step > 0 && step < 3 && (
          <Button variant="default" leftSection={<IconArrowLeft size={14} />} onClick={() => setStep((step - 1) as Step)}>
            Back
          </Button>
        )}
        {step === 3 && !running && record?.state !== "succeeded" && (
          <Button variant="default" leftSection={<IconArrowLeft size={14} />} onClick={() => setStep(2)}>
            Back to review
          </Button>
        )}
        <span className="min-w-0 flex-1" />
        {step === 0 && (
          <Button
            rightSection={loading ? undefined : <IconArrowRight size={14} />}
            loading={loading}
            disabled={loadedMirror !== mirror}
            onClick={() => setStep(1)}
          >
            Continue
          </Button>
        )}
        {step === 1 && (
          <Button rightSection={<IconArrowRight size={14} />} disabled={!release} onClick={() => setStep(2)}>
            Review
          </Button>
        )}
        {step === 2 && release && (
          <Button
            leftSection={<IconDownload size={14} />}
            disabled={release.installed && (!activate || alreadyDefault)}
            onClick={install}
          >
            {release.installed ? (activate && !alreadyDefault ? `Switch to ${release.version}` : "Nothing to do") : `Install ${release.version}`}
          </Button>
        )}
        {step === 3 && !running && (
          <Button onClick={onClose}>{record?.state === "succeeded" ? "Done" : "Close"}</Button>
        )}
      </div>
    </Modal>
  );
}
