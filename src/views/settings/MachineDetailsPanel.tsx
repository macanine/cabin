import { Button } from "@mantine/core";
import type { ReactNode } from "react";
import { Section, StateIcon, type StateTone } from "../../components/common";
import { formatBytes, formatDuration, formatInteger, formatMs, formatPercent, formatRate } from "../../lib/format";
import { selectNetworkSummary } from "../../lib/networkSummary";
import { formatDateTime, formatGuestDateTime } from "../../lib/time";
import type { DeviceDetails, DeviceReportState, MachineDetails } from "../../services/ipc/generated/common";
import type { GuestMachineObservation, MachineStatus } from "../../services/ipc/generated/core";
import { useApp } from "../../state/store";
import { uiActions } from "../../state/ui";

type Fact = [label: string, value: ReactNode];

function Facts({ rows }: { rows: Fact[] }) {
  return <dl className="machine-facts">{rows.map(([label, value]) => (
    <div key={label}>
      <dt>{label}</dt><dd>{value ?? <span className="faint">Unavailable</span>}</dd>
    </div>
  ))}</dl>;
}

const mono = (value: ReactNode) => value == null ? null : <span className="mono">{value}</span>;
const date = (value: number | null | undefined) => value == null ? null : formatDateTime(value);
const disk = (value: string | null) => value?.replace(/^(\d+(?:\.\d+)?)([KMGT])$/i, "$1 $2iB") ?? null;
const rate = (value: number | null | undefined) => value == null ? null : formatRate(value);

export function MachineResources({ machine, observation }: { machine: MachineStatus; observation: GuestMachineObservation | null }) {
  const sample = useApp((state) => state.monitor[state.monitor.length - 1]);
  const connected = machine.connection === "connected";
  const live = connected ? sample : undefined;
  const memory = live?.memoryUsed != null && live.memoryTotal != null
    ? `${formatBytes(live.memoryUsed)} of ${formatBytes(live.memoryTotal)}` : null;
  const filesystem = observation?.rootAvailableBytes != null && observation.rootTotalBytes != null
    ? `${formatBytes(observation.rootAvailableBytes)} available of ${formatBytes(observation.rootTotalBytes)}` : null;
  return (
    <Section title="Resources" hint={live ? `Sampled ${formatGuestDateTime(live.at)}` : connected ? "Waiting for a live sample" : "Live metrics are offline"}>
      <Facts rows={[
        ["Allocated CPU", machine.cpus == null ? null : `${formatInteger(machine.cpus)} vCPUs`],
        ["Allocated memory", machine.memoryMb == null ? null : formatBytes(machine.memoryMb * 1024 ** 2)],
        ["Virtual disk capacity", disk(machine.diskSize)],
        ["Guest root filesystem", filesystem],
        ...(live ? [
          ["CPU load", formatPercent(live.cpuPercent)],
          ["Memory used", memory],
          ["Disk read", rate(live.diskReadBps)], ["Disk write", rate(live.diskWriteBps)],
          ["Network received", rate(live.downloadBps)], ["Network sent", rate(live.uploadBps)],
          ["TCP latency", formatMs(live.latencyMs)],
        ] satisfies Fact[] : []),
      ]} />
    </Section>
  );
}

const DEVICE_STATES: Record<DeviceReportState, [string, StateTone]> = {
  missing: ["No report yet", "pending"],
  stale: ["Report is for an older configuration", "warn"],
  unverified: ["Evidence incomplete", "warn"],
  failed: ["Verification failed", "bad"],
  warnings: ["Verified with warnings", "warn"],
  passed: ["Verified", "ok"],
};

const GROUPS: Record<string, string> = {
  cpu: "CPU", smbios: "Firmware and SMBIOS", devices: "Devices", network: "Network", guest: "Guest system",
};

function DeviceProfile({ device }: { device: DeviceDetails | null }) {
  if (!device) return <Section title="Device profile"><p className="muted machine-note">Disabled</p></Section>;
  const [label, tone] = DEVICE_STATES[device.state];
  const groups = Object.entries(GROUPS).map(([id, name]) => ({
    name, findings: device.findings.filter((finding) => finding.group === id),
  }));
  const findingTone = (severity: string): StateTone =>
    severity === "error" ? "bad" : severity === "warning" || severity === "unverified" ? "warn" : "ok";
  return (
    <Section title="Device profile">
      <div className="row machine-device-state"><StateIcon tone={tone} /><strong>{label}</strong></div>
      <Facts rows={[
        ["Profile", device.preset], ["Profile CPU", device.cpu], ["Storage controller", device.storage],
        ["Network adapter", device.nic], ["Hardware phase", device.runtime ? "Runtime" : "Installation"],
        ["Sealed", device.sealed ? "Yes" : "No"], ["Last measured", date(device.measuredAt)],
        ["Report on Mac", mono(device.reportPath)],
      ]} />
      {groups.filter(({ findings }) => findings.length).map(({ name, findings }) => {
        const issues = findings.filter((finding) => finding.severity !== "info").length;
        const count = issues || findings.length;
        const type = issues ? "finding" : "check";
        return <details key={name} className="machine-expander" open={issues ? true : undefined}>
          <summary>{name} <span className="faint">{formatInteger(count)} {type}{count === 1 ? "" : "s"}</span></summary>
          <ul className="machine-findings">
            {findings.map((finding) => <li key={finding.id}>
              <StateIcon tone={findingTone(finding.severity)} size={14} />
              <span>{finding.title}{finding.residual && <span className="muted"> · Known platform trait</span>}</span>
            </li>)}
          </ul>
        </details>;
      })}
    </Section>
  );
}

export function MachineDetailsPanel({ machine, details, observation }: {
  machine: MachineStatus; details: MachineDetails; observation: GuestMachineObservation | null;
}) {
  const summary = useApp(selectNetworkSummary);
  const network = useApp((state) => state.networkLoaded ? state.network : null);
  const nodes = useApp((state) => state.proxy.nodes);
  const environment = useApp((state) => machine.connection === "connected" ? state.environment : null);
  const exit = network?.report?.revision === network?.revision
    ? network?.report?.checks.find((check) => check.id === "exit") : null;
  const route = network?.route;
  const routeText = route?.enabled ? route.chain.map((id) => nodes.find((node) => node.id === id)?.name ?? "Unavailable node").join(" → ") : route ? "Direct" : null;
  return (
    <>
      <div className="machine-columns">
        <Section title="System" hint={observation ? `Guest observed ${formatGuestDateTime(observation.at)}` : undefined}>
          <Facts rows={[
            ["Operating system", `${details.osName} · ${details.imageUrl?.includes("amd64") ? "x86_64" : "ARM64"}`],
            ["Login", machine.username && machine.hostname ? mono(`${machine.username}@${machine.hostname}`) : null],
            ["Guest kernel", mono(observation?.kernel)],
            ["Guest uptime", observation ? formatDuration(observation.uptimeSeconds * 1000) : null],
            ["Guest agent", mono(machine.guestVersion)],
            ["Engine", details.qemuRelease ?? "QEMU"], ["Acceleration", details.accelerator.toUpperCase()],
            ["QEMU PID", details.pid == null ? null : mono(String(details.pid))],
            ["QEMU started", date(details.startedAt)],
          ]} />
        </Section>
        <MachineResources machine={machine} observation={observation} />
        <Section title="Network and region" hint={`${summary.title} · ${summary.detail}`}>
          <Facts rows={[
            ["VM network mode", details.networkMode], ["SSH endpoint", mono(details.sshEndpoint)], ["MAC address", mono(details.mac)],
            ["Route", routeText], ["Mac network interface", route ? route.bindInterface ?? "System network" : null],
            ["Last network check", date(network?.checkedAt)], ["Authorization expires", date(network?.expiresAt)],
            ["Time zone", environment?.systemTimezone ?? details.timezone],
            ["Language", environment?.systemLanguage ?? details.locale],
          ]} />
          {exit && <p className="machine-note muted">{exit.summary}</p>}
          <Button variant="subtle" size="compact-sm" onClick={() => uiActions.navigate({ kind: "settings", tab: "network" })}>Network settings</Button>
        </Section>
        <Section title="Display and Chrome">
          <Facts rows={[
            ["Desktop", details.desktop ?? "Not installed"], ["QEMU display", details.displayMode],
            ["Desktop resolution", details.desktop ? details.resolution : null],
            ["Desktop scale", details.desktop ? `${details.scale}×` : null],
            ["Graphics adapter", details.gpu], ["Audio backend", details.audio],
            ["Viewers", details.desktop ? details.spice ? "VNC and SPICE" : "VNC" : null],
            ["Chrome", machine.browser ? "Configured" : "Not configured"],
          ]} />
          {machine.browser && machine.connection === "connected" && (
            <Button variant="subtle" size="compact-sm" onClick={() => uiActions.navigate({ kind: "chrome" })}>Open Chrome view</Button>
          )}
        </Section>
      </div>
      <Section title="Image and files">
        <Facts rows={[
          ["Cloud image", mono(details.imageUrl)], ["Image digest", mono(details.imageChecksum)],
          ["Package mirror", mono(details.aptMirror) ?? "Image default"],
          ["Disk image", mono(details.diskPath)], ["Disk space on Mac", details.diskAllocatedBytes == null ? null : formatBytes(details.diskAllocatedBytes)],
          ["Instance folder", mono(details.instancePath)], ["Log folder", mono(details.logsPath)],
        ]} />
      </Section>
      <DeviceProfile device={details.device} />
    </>
  );
}
