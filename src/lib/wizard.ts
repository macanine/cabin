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

// Match specific causes before their enclosing step or a generic I/O error.
const FAILURE_RULES: readonly (readonly [RegExp, string, string])[] = [
  [/not enough free (disk )?space|not enough free disk|no space left|not enough space on the disk|storage is full|disk quota exceeded/,
    "Not enough free disk space", "Free up space on the volume that holds Cabin’s data or choose a smaller VM disk, then retry."],
  [/could not allocate memory|cannot allocate memory|out of memory|not enough memory/,
    "The VM could not get enough memory", "Close apps that use a lot of memory or choose a smaller VM memory size, then retry."],
  [/another vmctl operation is in progress|already in progress|finish the current operation|wait for the machine startup attempt/,
    "Another machine operation is already running", "Wait for the current operation to finish, then retry."],
  [/download cancelled|install cancelled|cabin is quitting/,
    "The download was cancelled", "Open Cabin again and retry the install. Cabin can reuse a verified image or a saved partial download."],
  [/tls verification or handshake failed|invalid peer certificate|unknownissuer|certificate verify failed|certificate has expired/,
    "Could not establish a secure download connection", "Check this computer’s date and time. If your network uses an inspecting proxy, check that its certificate is trusted on this computer."],
  [/captive portal|received an html page/,
    "The network sent a sign-in page instead of the download", "Open a browser and sign in to the network if prompted, then retry."],
  [/download dns lookup failed|failed to look up address information/,
    "Could not find the download server", "Check this computer’s internet connection, DNS, or proxy, then retry."],
  [/download stalled|connection dropped while downloading.*timed out/,
    "The download stopped receiving data", "Check this computer’s connection or proxy, then retry. Cabin can continue from the saved partial download."],
  [/http 407|proxy authentication|invalid proxy url|could not connect to (?:the )?proxy/,
    "The download proxy could not connect", "Check the proxy address and credentials on this computer, then retry."],
  [/--user must|--hostname must|(?:username|hostname) “/,
    "Check the VM username and hostname", "Use a lowercase Linux username and a hostname with lowercase letters, digits, or hyphens."],
  [/invalid ssh public key|generated ssh public key|ssh-keygen produced an unexpected public key/,
    "The guest’s SSH public key could not be prepared", "Retry to generate a fresh key for the VM. If it fails again, open the seed step log."],
  [/--disk-size|invalid size:|could not check free disk space/,
    "The VM disk size could not be checked", "Check the available space on the volume that holds Cabin’s data and retry with a valid disk size."],
  [/qemu-img[^\n]*(?:permission denied|access is denied|being used by another process|sharing violation)/,
    "The VM disk file could not be opened for writing", "Check that Cabin can write to its data folder and that no other program is holding the file, then retry."],
  [/qemu-img[^\n]*(?:failed \(exit|timed out while preparing)|could not read base image|new qcow2 metadata|wrong virtual size/,
    "The VM disk could not be prepared", "Check the disk step log for the tool’s error, then retry."],
  [/sha256sums|no valid image checksum/,
    "Ubuntu’s image checksum list could not be verified", "Retry when the mirror finishes syncing. If the problem continues, copy diagnostics to share with Cabin support."],
  [/could not find a[^\n]* image|builds are listed|lists an (?:arm64 )?image|no image build is pinned|base image missing/,
    "The system image could not be found", "Ubuntu’s image index could not be read. Check this computer’s internet connection or proxy, then retry."],
  [/embedded image checksum mismatch|copy embedded image failed/,
    "The included Ubuntu image could not be prepared", "Reinstall Cabin if the included image is damaged. Check the image step log for any storage errors before retrying."],
  [/checksum mismatch|sha256 mismatch|wrong checksum|failed to verify (?:the )?(?:download|image)/,
    "The downloaded image did not match its checksum", "Retry the download. If it keeps failing, copy diagnostics so Cabin support can check the pinned image."],
  [/qemu (?:archive|.*firmware)|macanine\/qemu/,
    "The virtual machine engine could not be prepared", "Cabin’s QEMU or its firmware could not be checked or unpacked. Retry, then reinstall Cabin if it keeps failing."],
  [/qemu[^\n]*(?:does not run|could not launch|os error (?:225|226|577|1260))|could not launch qemu|blocked by group policy/,
    "Cabin’s bundled QEMU could not run", "Check your computer’s security or antivirus history for a block on the official Cabin build, then recheck setup or reinstall Cabin."],
  [/(?:qemu(?:-system[-\w]*|-img)?\s+(?:is\s+)?(?:not installed|not found|missing|not available)|bundled qemu is missing)/,
    "Cabin’s bundled QEMU is missing", "Check the security history for a quarantined Cabin file, then reinstall Cabin and open it again."],
  [/required tool missing: ssh|command not found:[^\n]*ssh(?:-keygen)?\b/,
    "The SSH tools could not be found", "Reinstall Cabin on Windows. On other systems, restore the SSH tools included with the operating system, then retry."],
  [/download failed after|could not (?:download|reach https?:|connect:)|download (?:content-length|server rejected|exceeded its declared length)|invalid download content-length|no download source|connection (?:reset|dropped|closed early)|http [45]\d\d/,
    "A file could not be downloaded", "Check this computer’s internet connection or proxy, then retry. Cabin can resume a partial image download and try other mirrors."],
  [/temporary failure resolving|could not resolve|network is unreachable|apt does not report the configured proxy|proxy egress lock failed|ipv4-only guest networking|nic .* has no ipv4 address/,
    "The VM could not reach the internet", "Check this computer’s internet connection and the proxy or network settings in Cabin, then retry."],
  [/debian is no longer supported/,
    "This machine needs Ubuntu", "Back up what you need from the old machine before replacing it with a new Ubuntu installation in Machine settings."],
  [/unknown operating system|--cpus must|--memory must|--apt-mirror must|--ssh-port must/,
    "Check the machine settings", "Open Advanced and choose a supported Ubuntu release, size, and package mirror, then retry."],
  [/device profile is currently built for arm/,
    "The device profile is unavailable on this computer", "Turn off Device profile under Advanced, then retry."],
  [/already installed or has files to recover|already provisioned with a different configuration|existing disk .* needs recovery|disk was kept|files were kept|cleaning up the partial install also failed|seed is immutable/,
    "The existing machine needs recovery", "Open Machine settings and use Verify or repair installation. Keep a backup before choosing to erase anything."],
  [/stop the machine before repairing|previous qemu process has not exited|cannot identify the recorded qemu process|machine could not be stopped/,
    "A previous machine process is still running", "Stop the machine in Cabin. If it cannot stop, restart this computer before trying again."],
  [/earlier version of cabin is still running with tcp control sockets/,
    "The previous version still has the machine running", "Stop the machine in that version of Cabin or restart this computer, then try again."],
  [/vm socket path (?:is too long|must be valid utf-8)|filename or extension is too long|file name too long/,
    "The machine’s data path cannot be used", "Use a shorter local profile path with ordinary characters, or ask your administrator to change the location of Cabin’s data."],
  [/unexpected file at vm socket path|could not determine whether the vm socket|process is already listening at/,
    "A machine control socket is in use", "Close other copies of Cabin and restart this computer. If the error names an unexpected file, move that file out of the way before retrying."],
  [/qemu could not bind the ssh forwarding port|host forwarding rule/,
    "The VM’s SSH port could not be opened", "Close the program using that port, then retry."],
  [/another process has the vm disk open|failed to get "write" lock|another process using the image/,
    "Another process has the VM disk open", "Stop the other VM or close the program holding the disk, then retry."],
  [/ineligible for hvf|could not use hardware virtualization \(hvf\)|failed to initialize hvf|hvf error|hypervisor entitlement/,
    "QEMU could not use Apple’s virtualization hardware", "Install a fresh copy of Cabin in Applications, then retry."],
  [/whvcreatepartition failed|whpx.*(?:failed|not available)|invalid accelerator whpx/,
    "Windows hardware acceleration could not start", "Check that virtualization is enabled in firmware and Windows Hypervisor Platform is turned on. Restart Windows and retry."],
  [/qmp|qemu control request timed out|monitor did not report a running machine|guest-agent .*failed|guest-agent connection closed/,
    "The machine’s control connection did not respond", "Stop the machine if it is running, then retry. If the problem repeats, copy diagnostics to share with Cabin support."],
  [/qemu exited|qemu.*failed to start/,
    "The VM engine stopped during startup", "Check the startup and guest console logs for the cause, then retry."],
  [/remote host identification has changed|host key verification failed/,
    "The VM’s SSH host key changed", "Stop the machine and use Verify or repair installation in Machine settings. Make sure the SSH port belongs to Cabin before retrying."],
  [/ssh did not become ready|waiting for ssh|ssh connection timed out/,
    "The VM did not become ready for SSH", "Check the guest console for boot errors, then retry. Allow extra time when the machine is running with software emulation."],
  [/could not deploy cabind|timed out deploying|guest helper sha-256|unsafe helper directory|could not load the initial guest snapshot|running build.*does not match|hello.*(?:mismatch|failed)/,
    "Cabin’s service could not start in the VM", "Retry the startup. If it still fails, check the start log and verify the installation in Machine settings."],
  [/cabind (?:binary not found|for the vm was not found)|vm service is missing/,
    "Cabin’s VM service is missing", "Install a fresh copy of Cabin, then retry."],
  [/ssh authentication failed|permission denied \(publickey\)|missing ssh key|ssh connection refused|ssh: connect to host/,
    "Could not log in to the VM over SSH", "Open the guest console and check that the VM booted. If retrying fails, verify the installation in Machine settings."],
  [/access is denied|permission denied|read-only file system|cannot write vmctl_home|qemu was denied access|sharing violation/,
    "Cabin could not access a required file", "Check access to the folder where Cabin stores its data and close other apps using the file, then retry."],
  [/timed out|timeout/,
    "The VM took too long to respond", "Close apps that use a lot of CPU or memory and check the guest console before retrying."],
  [/cloud-init did not finish|cloud-init status is not done|acceptance script failed|passwordless sudo for the provisioned user|kernel metapackage|qemu-guest-agent package is not installed/,
    "Ubuntu could not finish its initial setup", "Check the first-boot log for a package or network error. Retry, or choose another package mirror under Advanced."],
  [/json error|saved machine configuration|unable to read.*config|config (?:defaults|overrides) must be/,
    "Cabin could not read the machine’s configuration", "Open Machine settings to verify the installation, or copy diagnostics if the configuration still cannot be read."],
  [/preflight failed|unsupported host architecture/,
    "This computer did not pass the setup checks", "Return to setup and check the readiness messages before retrying."],
];

const STEP_FAILURES: Record<string, FailureHelp> = {
  qemu: { title: "The virtual machine engine could not be prepared", advice: "Check the engine step log, then retry or reinstall Cabin." },
  image: { title: "The system image could not be prepared", advice: "Open the image step log for details, then retry." },
  disk: { title: "The VM disk could not be prepared", advice: "Check the disk step log and that Cabin can write to its data folder, then retry." },
  seed: { title: "The VM’s initial configuration could not be prepared", advice: "Open the seed step log for details, then retry." },
  provision: { title: "Ubuntu could not finish its initial setup", advice: "Check the first-boot log for a package or network error, then retry." },
  desktop: { title: "The Ubuntu desktop could not be set up", advice: "Check the desktop step log for a package or network error, then retry." },
  browser: { title: "The guest browser could not be set up", advice: "Check the browser step log, then retry." },
  "device-network": { title: "The device network could not be configured", advice: "Check Cabin’s network settings and the device network log, then retry." },
  agentd: { title: "Cabin’s service could not start in the VM", advice: "Check the start log, then verify the installation in Machine settings." },
};

/** Explain a host or guest error in the context of the failed install step. */
export function explainFailure(text: string, step?: string, log: readonly string[] = []): FailureHelp {
  const t = text.toLowerCase();
  const port = t.match(/(?:tcp|ssh) port (\d+)[^\n]*already in use/);
  if (port) return { title: `Port ${port[1]} is already in use`, advice: "Close the application using the VM’s SSH port, then retry." };
  // A launcher can return only a command and exit code. Its stderr is in the step log.
  const output = /command failed \(exit/.test(t) ? `${t}\n${log.join("\n").toLowerCase()}` : t;
  const rule = FAILURE_RULES.find(([pattern]) => pattern.test(output));
  if (rule) return { title: rule[1], advice: rule[2] };

  const id = step ?? t.match(/step "([^"]+)" failed/)?.[1];
  if (id && Object.prototype.hasOwnProperty.call(STEP_FAILURES, id)) return STEP_FAILURES[id];
  if (id?.startsWith("device-")) {
    return { title: "The device profile could not be applied", advice: "Check the failed step log, or turn off Device profile under Advanced and retry." };
  }
  return {
    title: "A step failed",
    advice: "Open Details to check the failed step, then retry. Copy diagnostics if you need to share the error.",
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
