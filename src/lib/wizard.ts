// Pure mapping from pipeline steps to the installation wizard's steps.
import type { InstallStep, MachineStatus } from "../services/ipc/generated/core";

export type WizardState = "pending" | "running" | "done" | "cached" | "failed" | "rolledBack";

export interface WizardStepDef {
  key: string;
  title: string;
  description: string;
  helper: string;
  /** Pipeline step ids that make up this wizard step. */
  pipeline: string[];
}

export const WIZARD_STEPS: WizardStepDef[] = [
  {
    key: "image",
    title: "Download Ubuntu",
    description: "The newest official ARM64 cloud image, checked against Ubuntu's published SHA-256.",
    helper: "A verified image is kept, so a retry or a reinstall skips the download.",
    pipeline: ["image"],
  },
  {
    key: "skeleton",
    title: "Prepare the virtual machine",
    description: "CPU, memory, network and UEFI firmware, run by the built-in QEMU.",
    helper: "QEMU is downloaded once and reused.",
    pipeline: ["qemu"],
  },
  {
    key: "disk",
    title: "Create the system disk",
    description: "A disk on top of the verified image, sized as chosen, plus the UEFI variable store.",
    helper: "The image itself is never changed; the disk holds this machine's changes.",
    pipeline: ["disk"],
  },
  {
    key: "seed",
    title: "Write the first-boot setup",
    description: "Your user, an SSH key, the hostname, networking and packages, read by cloud-init on first boot.",
    helper: "Nothing has to be typed into the machine: it configures itself.",
    pipeline: ["seed"],
  },
  {
    key: "provision",
    title: "First boot",
    description: "Ubuntu boots, installs its packages and is checked: user, hostname, network and proxy.",
    helper: "The longest step: it downloads the system's packages.",
    pipeline: ["device-cpu", "device-smbios", "device-devices", "device-network", "device-guest", "provision", "desktop", "browser", "device-seal"],
  },
  {
    key: "agentd",
    title: "Connect cabin",
    description: "cabind is copied in, started as a service and connected over SSH.",
    helper: "From here on, workspaces, sessions and agents live inside the machine.",
    pipeline: [],
  },
];

/** Aggregate state of one wizard step from its pipeline steps. */
export function wizardState(def: WizardStepDef, steps: InstallStep[], machine: MachineStatus | null): WizardState {
  if (def.key === "agentd") {
    if (!machine || machine.state !== "running") return "pending";
    if (machine.connection === "connected") return "done";
    return machine.busy ? "pending" : "running";
  }
  const own = steps.filter((step) => def.pipeline.includes(step.id));
  if (own.length === 0) return "pending";
  if (own.some((s) => s.state === "failed")) return "failed";
  if (own.some((s) => s.state === "rolledBack")) return "rolledBack";
  if (own.some((s) => s.state === "running")) return "running";
  if (own.every((s) => s.state === "cached")) return "cached";
  // `satisfied`/`needed` come from "Verify installation".
  if (own.every((s) => s.state === "done" || s.state === "cached" || s.state === "satisfied")) return "done";
  if (own.some((s) => s.state === "needed")) return "pending";
  return own.some((s) => s.state === "done") ? "running" : "pending";
}

/** The step to show by default: the running one, else the first not done. */
export function currentWizardStep(steps: InstallStep[], machine: MachineStatus | null): number {
  const states = WIZARD_STEPS.map((def) => wizardState(def, steps, machine));
  const running = states.findIndex((s) => s === "running" || s === "failed");
  if (running >= 0) return running;
  const next = states.findIndex((s) => s === "pending");
  return next >= 0 ? next : WIZARD_STEPS.length - 1;
}

export interface FailureHelp {
  title: string;
  advice: string;
}

/** Plain-language explanation of common install failures. */
export function explainFailure(text: string): FailureHelp {
  const t = text.toLowerCase();
  if (t.includes("could not find a") || t.includes("builds are listed") || t.includes("lists an arm64 image")) {
    return {
      title: "The system image could not be found",
      advice:
        "Ubuntu's image index could not be read. Check the Mac's internet connection or proxy, then retry.",
    };
  }
  if (t.includes("qemu archive") || t.includes("macanine/qemu")) {
    return {
      title: "The built-in QEMU could not be installed",
      advice: "Its download or unpacking failed. Check the Mac's internet connection and free disk space, then retry; the download resumes.",
    };
  }
  if (t.includes("download cancelled")) {
    return { title: "The download was cancelled", advice: "Install again to continue; the part already downloaded is kept and resumed." };
  }
  if (t.includes("download failed after") || /http [45]\d\d/.test(t)) {
    return {
      title: "A file could not be downloaded",
      advice:
        "The Ubuntu image or the built-in QEMU could not be downloaded. Check the Mac's internet connection or proxy, then retry. What was already downloaded is kept and resumed.",
    };
  }
  if (t.includes('step "device-network" failed')) {
    return {
      title: "The device network could not be configured",
      advice: "Check the network configuration and the log below, then retry the installation.",
    };
  }
  const portConflict = t.match(/tcp port (\d+).*already in use/);
  if (portConflict) {
    return {
      title: `Port ${portConflict[1]} is already in use`,
      advice: "Another application is using the VM's SSH port. Close it or change the VM's SSH port, then retry.",
    };
  }
  if (t.includes("temporary failure resolving") || t.includes("could not resolve") || t.includes("network is unreachable")) {
    return {
      title: "The VM could not reach the internet",
      advice: "Downloads from the package mirrors failed. Check the Mac's internet connection or proxy (Environment), then retry. Already downloaded files are reused.",
    };
  }
  if (t.includes("not enough free disk")) {
    return {
      title: "Not enough free disk space",
      advice: "Free up space on the Mac or choose a smaller disk size, then retry.",
    };
  }
  if (t.includes("checksum") || t.includes("sha256")) {
    return {
      title: "The downloaded image did not match its checksum",
      advice: "The download was corrupted or the image changed upstream. Retry; if it keeps failing, the pinned image needs updating.",
    };
  }
  if (t.includes("qemu") && (t.includes("not installed") || t.includes("not found") || t.includes("missing"))) {
    return {
      title: "The built-in QEMU is missing",
      advice: "Retry the installation: its QEMU step downloads and unpacks it again.",
    };
  }
  if (t.includes("timed out") || t.includes("timeout")) {
    return {
      title: "The VM took too long to respond",
      advice: "It may be short on CPU or memory. Close other heavy apps or give the VM more resources, then retry.",
    };
  }
  if (t.includes("ssh")) {
    return {
      title: "Could not log in to the VM over SSH",
      advice: "The VM booted but did not accept the generated key in time. Retry; if it persists, open the log below.",
    };
  }
  return {
    title: "A step failed",
    advice: "The partial machine was removed and nothing else was changed. Check the log below, then retry.",
  };
}

/** Overall progress, 0–1, over the wizard's six steps. */
export function overallProgress(states: WizardState[], fractions: (number | null)[] = []): number {
  // A running stage counts as half done, or its real fraction when known
  // (the image download).
  const weight: number[] = states.map((s, i) =>
    s === "done" || s === "cached" ? 1 : s === "running" ? (fractions[i] ?? 0.5) : 0,
  );
  return weight.reduce((a, b) => a + b, 0) / states.length;
}


/** The newest log line of a running stage, without its timestamp prefix. */
export function stageTicker(steps: InstallStep[], def: WizardStepDef): string | null {
  const own = steps.filter((step) => def.pipeline.includes(step.id));
  const running = own.find((step) => step.state === "running") ?? own[own.length - 1];
  const lines = running?.logTail ?? [];
  for (let i = lines.length - 1; i >= 0; i--) {
    const line = lines[i].replace(/^\[[^\]]*\]\s*/, "").trim();
    if (line) return line.length > 140 ? `${line.slice(0, 139)}…` : line;
  }
  return null;
}
