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
    key: "skeleton",
    title: "Check the virtual machine engine",
    description: "The QEMU and UEFI firmware that come with Cabin are checked before anything is created.",
    helper: "Nothing to install: the engine is part of Cabin.",
    pipeline: ["qemu"],
  },
  {
    key: "image",
    title: "Download Ubuntu",
    description: "The newest official Ubuntu cloud image from the fastest mirror, checked against Ubuntu's published SHA-256.",
    helper: "A verified image is kept, so a retry or a reinstall skips the download.",
    pipeline: ["image"],
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
    title: "Start Cabin in the machine",
    description: "Cabin's service is copied into the machine, started and connected.",
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
  if (/not enough free (?:disk|space)|no space left on device|not enough space on the disk/.test(t)) {
    return {
      title: "Not enough free disk space",
      advice: "Free up space on the volume that holds Cabin’s data or choose a smaller VM disk, then retry.",
    };
  }
  if (t.includes("another vmctl operation is in progress")) {
    return {
      title: "Another machine operation is already running",
      advice: "Wait for the current operation to finish, then retry.",
    };
  }
  if (t.includes("tls verification or handshake failed")) {
    return {
      title: "Could not establish a secure download connection",
      advice: "Check this computer’s date and time. If your network uses an inspecting proxy, check that its certificate is trusted on this computer.",
    };
  }
  if (t.includes("captive portal") || t.includes("received an html page")) {
    return {
      title: "The network sent a sign-in page instead of the download",
      advice: "Open a browser and sign in to the network if prompted, then retry.",
    };
  }
  if (t.includes("download dns lookup failed")) {
    return {
      title: "Could not find the download server",
      advice: "Check this computer’s internet connection, DNS, or proxy, then retry.",
    };
  }
  if (t.includes("download stalled")) {
    return {
      title: "The download stopped receiving data",
      advice: "Check this computer’s connection or proxy, then retry. Cabin can continue from the saved partial download.",
    };
  }
  if (t.includes("--user must") || t.includes("--hostname must") || /(?:username|hostname) “/.test(t)) {
    return {
      title: "Check the VM username and hostname",
      advice: "Use a lowercase Linux username and a hostname with lowercase letters, digits, or hyphens.",
    };
  }
  if (t.includes("invalid ssh public key") || t.includes("generated ssh public key") || t.includes("ssh-keygen produced an unexpected public key")) {
    return {
      title: "The guest’s SSH public key could not be prepared",
      advice: "Retry to generate a fresh key for the VM. If it fails again, open the seed step log.",
    };
  }
  if (t.includes("--disk-size") || t.includes("could not check free disk space")) {
    return {
      title: "The VM disk size could not be checked",
      advice: "Check the available space on the volume that holds Cabin’s data and retry with a valid disk size.",
    };
  }
  if (t.includes("qemu-img") && /permission denied|access is denied|being used by another process|sharing violation/.test(t)) {
    return {
      title: "The VM disk file could not be opened for writing",
      advice: "Check that Cabin can write to its data folder and that no other program is holding the file, then retry.",
    };
  }
  if (t.includes("qemu-img") && (t.includes("failed (exit") || t.includes("timed out while preparing"))) {
    return {
      title: "The VM disk could not be prepared",
      advice: "Check the disk step log for the tool’s error, then retry.",
    };
  }
  if (t.includes("could not find a") || t.includes("builds are listed") || t.includes("lists an arm64 image") || t.includes("lists an image for this architecture") || t.includes("no valid image checksum") || t.includes("no image build is pinned")) {
    return {
      title: "The system image could not be found",
      advice:
        "Ubuntu's image index could not be read. Check this computer's internet connection or proxy, then retry.",
    };
  }
  if (t.includes("qemu archive") || t.includes("macanine/qemu") || t.includes("bundled qemu")) {
    return {
      title: "The virtual machine engine could not be prepared",
      advice: "Cabin's bundled QEMU could not be checked or unpacked. Retry; if it keeps failing, reinstall Cabin.",
    };
  }
  if (t.includes("download cancelled")) {
    return { title: "The download was cancelled", advice: "Install again to continue; the part already downloaded is kept and resumed." };
  }
  if (t.includes("download failed after") || t.includes("could not download") || t.includes("download content-length") || t.includes("invalid download content-length") || t.includes("download server rejected") || t.includes("download exceeded its declared length") || t.includes("no download source") || t.includes("connection reset") || /http [45]\d\d/.test(t)) {
    return {
      title: "A file could not be downloaded",
      advice:
        "The Ubuntu image could not be downloaded. Check this computer's internet connection or proxy, then retry: the download resumes where it stopped and tries the other mirrors.",
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
      advice: "Downloads from the package mirrors failed. Check this computer's internet connection or proxy (Environment), then retry. Already downloaded files are reused.",
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
      title: "Cabin's bundled QEMU is missing",
      advice: "Cabin's files are damaged or incomplete. Reinstall Cabin, then open it again.",
    };
  }
  if (t.includes('step "disk" failed') || t.includes("could not read base image")) {
    return {
      title: "The VM disk could not be prepared",
      advice: "Check the disk step log and that Cabin can write to its data folder, then retry.",
    };
  }
  if (t.includes('step "seed" failed')) {
    return {
      title: "The VM’s initial configuration could not be prepared",
      advice: "Open the seed step log for details, then retry.",
    };
  }
  if (t.includes('step "qemu" failed')) {
    return {
      title: "The virtual machine engine could not be prepared",
      advice: "Open the engine step log for details, then retry.",
    };
  }
  if (t.includes('step "image" failed') || t.includes("copy embedded image failed")) {
    return {
      title: "The system image could not be prepared",
      advice: "Open the image step log for details, then retry.",
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
