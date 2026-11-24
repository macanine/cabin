import type { ReactNode } from "react";
import type { HostFacts } from "../../services/ipc/generated/common";

export const gb = (mb: number) => `${mb / 1024 % 1 === 0 ? mb / 1024 : (mb / 1024).toFixed(1)} GB`;

export type CardTone = "draft" | "building" | "ready" | "failed" | "idle";

export interface MachineSpec {
  osName: string;
  cpus: number | null;
  memoryMb: number | null;
  diskGb: number | null;
  username: string;
  hostname: string;
}

/** One resource as a value and, when the host is known, a share of it. */
function Meter({ label, value, share, of }: { label: string; value: string; share: number | null; of?: string }) {
  return (
    <div className="iw-meter">
      <div className="iw-meter-row">
        <span>{label}</span>
        <strong>{value}</strong>
      </div>
      {share != null && (
        <div className="iw-meter-track" aria-hidden>
          <span style={{ inlineSize: `${Math.round(Math.max(0.04, Math.min(1, share)) * 100)}%` }} />
        </div>
      )}
      {of && <span className="iw-meter-of">{of}</span>}
    </div>
  );
}

const TONE_LABEL: Record<CardTone, string> = {
  draft: "Preview",
  building: "Installing",
  ready: "Running",
  failed: "Install stopped",
  idle: "Installed",
};

/**
 * The machine being configured or built, as one card: a shell prompt with
 * its user and hostname, its release, and how much of this computer it takes.
 * It stays beside both phases so the setup and the install read as one object.
 */
export function MachineCard({ spec, host, tone, caption, screen, badge, children }: {
  spec: MachineSpec;
  caption?: ReactNode;
  /** A live line under the prompt, such as the newest log line. */
  screen?: string | null;
  host: HostFacts | null;
  tone: CardTone;
  badge?: ReactNode;
  /** The card's actions, or null for none. */
  children?: ReactNode;
}) {
  const cpuShare = host?.cpus && spec.cpus ? spec.cpus / host.cpus : null;
  const memShare = host?.memoryMb && spec.memoryMb ? spec.memoryMb / host.memoryMb : null;
  const diskShare = host?.maxDiskGb && spec.diskGb ? spec.diskGb / host.maxDiskGb : null;
  return (
    <section className="iw-card" data-tone={tone} aria-label="Your machine">
      <div className="iw-card-screen" aria-hidden>
        <div className="iw-card-dots"><span /><span /><span /></div>
        <code className="iw-prompt">
          <span className="iw-prompt-user">{spec.username}@{spec.hostname}</span>
          <span className="iw-prompt-path">:~$</span>
          {!screen && <span className="iw-caret" />}
        </code>
        {screen && <code key={screen} className="iw-screen-line">{screen}<span className="iw-caret" /></code>}
      </div>
      <div className="iw-card-body">
        <div className="iw-card-head">
          <div className="min-w-0">
            <div className="iw-card-os">{spec.osName}</div>
            {caption && <div className="iw-card-sub">{caption}</div>}
          </div>
          <span className="iw-status" data-tone={tone}>
            <span className="iw-status-dot" />
            {badge ?? TONE_LABEL[tone]}
          </span>
        </div>
        <div className="iw-meters">
          <Meter label="CPU" value={spec.cpus ? `${spec.cpus} vCPU` : "—"} share={cpuShare} of={host?.cpus ? `of ${host.cpus}` : undefined} />
          <Meter label="Memory" value={spec.memoryMb ? gb(spec.memoryMb) : "—"} share={memShare} of={host?.memoryMb ? `of ${gb(host.memoryMb)}` : undefined} />
          <Meter label="Disk" value={spec.diskGb ? `${spec.diskGb} GB` : "—"} share={diskShare} of={host?.maxDiskGb ? `of ${host.maxDiskGb} GB available` : undefined} />
        </div>
        {children && <div className="iw-card-foot">{children}</div>}
      </div>
    </section>
  );
}
