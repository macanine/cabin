import assert from "node:assert/strict";
import test from "node:test";
import { currentWizardStep, explainFailure, WIZARD_STEPS, wizardState } from "../src/lib/wizard.ts";

const step = (id, state) => ({ id, title: id, state, detail: null, logTail: [] });
const def = (key) => WIZARD_STEPS.find((d) => d.key === key);
const machine = (over = {}) => ({
  state: "notInstalled", connection: "offline", busy: "Installing the machine", sshPort: null, os: null,
  cpus: null, memoryMb: null, diskSize: null, display: false, browser: false, device: false, guestVersion: null,
  error: null, ...over,
});

test("the four platform steps map to their pipeline steps", () => {
  const steps = [step("qemu", "done"), step("image", "cached"), step("disk", "running"), step("seed", "pending")];
  assert.equal(wizardState(def("image"), steps, machine()), "cached");
  assert.equal(wizardState(def("skeleton"), steps, machine()), "done");
  assert.equal(wizardState(def("disk"), steps, machine()), "running");
  assert.equal(wizardState(def("seed"), steps, machine()), "pending");
  assert.equal(currentWizardStep(steps, machine()), 2);
});

test("provisioning aggregates optional steps and failures win", () => {
  const steps = [step("provision", "done"), step("desktop", "running")];
  assert.equal(wizardState(def("provision"), steps, machine()), "running");
  assert.equal(wizardState(def("provision"), [step("provision", "failed"), step("desktop", "pending")], machine()), "failed");
});

test("the agent step follows the connection", () => {
  assert.equal(wizardState(def("agentd"), [], machine({ state: "running", busy: null, connection: "updating" })), "running");
  assert.equal(wizardState(def("agentd"), [], machine({ state: "running", busy: null, connection: "connected" })), "done");
});

// One case per rule in FAILURE_RULES, then the step fallbacks.
const failures = [
  ["preflight failed: not enough free disk: 1.9 GiB", /disk space/],
  ["QEMU could not allocate memory", /enough memory/],
  ["another vmctl operation is in progress", /already running/],
  ["download cancelled", /cancelled/],
  ["could not find a Ubuntu 24.04 LTS image: TLS verification or handshake failed", /secure download/],
  ["download received an HTML page instead of the file", /sign-in page/],
  ["download DNS lookup failed: Dns Failed", /find the download server/],
  ["download stalled (no bytes received for 30 seconds)", /stopped receiving data/],
  ["HTTP 407 Proxy Authentication Required", /proxy could not connect/],
  ["--user must be a valid lowercase Linux username", /username and hostname/],
  ["invalid SSH public key: expected one OpenSSH ed25519 public key", /public key could not be prepared/],
  ["could not check free disk space at C:/x: Access is denied", /size could not be checked/],
  ["qemu-img convert failed (exit 1): Permission denied", /opened for writing/],
  ["qemu-img resize failed (exit 1): unexpected error", /disk could not be prepared/],
  ["no valid image checksum in https://mirror.test/SHA256SUMS", /checksum list/],
  ["could not find a Ubuntu 26.04 LTS image: HTTP 404", /system image/],
  ["embedded image checksum mismatch: abcd", /included Ubuntu image/],
  ["checksum mismatch for https://mirror.test: expected a, got b", /did not match its checksum/],
  ["could not unpack the QEMU archive: invalid header", /engine could not be prepared/],
  ["Could not launch QEMU: Operation did not complete successfully (os error 225)", /QEMU could not run/],
  ["Cabin's bundled QEMU is missing or damaged.", /bundled QEMU is missing/],
  ["required tool missing: ssh-keygen", /SSH tools/],
  ["download Content-Length does not match Content-Range", /could not be downloaded/],
  ["Temporary failure resolving 'ports.ubuntu.com'", /internet/],
  ["debian-12, and Debian is no longer supported", /needs Ubuntu/],
  ["--cpus must be between 1 and 64", /machine settings/],
  ["The device profile is currently built for ARM machines", /device profile is unavailable/],
  ["The machine is already installed or has files to recover", /needs recovery/],
  ["Cannot identify the recorded QEMU process (PID 42).", /previous machine process/],
  ["An earlier version of Cabin is still running with TCP control sockets.", /previous version/],
  ["VM socket path is too long (110 bytes, maximum 103)", /data path/],
  ["Unexpected file at VM socket path x", /control socket/],
  ["QEMU could not bind the SSH forwarding port.", /port could not be opened/],
  ["Another process has the VM disk open.", /process has the VM disk/],
  ["QEMU could not use hardware virtualization (HVF).", /virtualization hardware/],
  ["WHvCreatePartition failed with 0x80070005", /Windows hardware acceleration/],
  ["QEMU control request timed out", /control connection/],
  ["QEMU exited during startup (exit status: 1)", /engine stopped/],
  ["REMOTE HOST IDENTIFICATION HAS CHANGED!", /host key changed/],
  ["SSH did not become ready within 180s", /ready for SSH/],
  ["Timed out deploying cabind in the VM", /service could not start/],
  ["Guest cabind binary not found at /other/cabind.", /service is missing/],
  ["dev@localhost: Permission denied (publickey).", /log in.*SSH/],
  ["cannot write VMCTL_HOME x: Permission denied", /access a required file/],
  ["timed out after 900s waiting for cloud-init", /took too long/],
  ["cloud-init did not finish cleanly (rc=2)", /initial setup/],
  ["JSON error: expected value at line 1 column 1", /configuration/],
  ["preflight failed: unsupported host architecture: x86", /setup checks/],
  ["step \"image\" failed: Input/output error", /image could not be prepared/],
  ["step \"device-guest\" failed: profile verification failed", /device profile/],
  ["something odd", /step failed/],
];

test("failures are explained in plain language, one rule per cause", () => {
  for (const [error, title] of failures) {
    const help = explainFailure(error);
    assert.match(help.title, title, error);
    assert.ok(help.advice.length > 0, error);
  }
  // A specific cause wins over the enclosing step and the SSH port is named.
  assert.match(explainFailure('step "image" failed: not enough free space to download to image.img.part').title, /disk space/);
  assert.match(explainFailure("preflight failed: TCP port 12233 on 127.0.0.1 is already in use").title, /Port 12233 is already in use/);
});

test("exit-code-only failures use the actual stderr from the step", () => {
  const help = explainFailure("command failed (exit 1): qemu-img resize", "disk", ["qemu-img: No space left on device"]);
  assert.match(help.title, /disk space/);
  assert.match(explainFailure("other failure", "browser", []).advice, /browser step log/);
});

test("overall progress uses fractional download progress when available", async () => {
  const { overallProgress } = await import("../src/lib/wizard.ts");
  assert.equal(overallProgress(["done", "cached", "running", "pending", "pending", "pending"]), 2.5 / 6);
  assert.equal(overallProgress(["done", "cached", "running", "pending", "pending", "pending"], [0.6]), 2.5 / 6);
  assert.equal(overallProgress(["done", "cached", "running", "pending", "pending", "pending"], [null, null, 0.6]), 2.6 / 6);
});

test("verify results", async () => {
  const verified = [step("qemu", "satisfied"), step("image", "satisfied"), step("disk", "satisfied"), step("seed", "needed"), step("provision", "needed")];
  assert.equal(wizardState(def("disk"), verified, machine({ busy: null, state: "stopped" })), "done");
  assert.equal(wizardState(def("seed"), verified, machine({ busy: null, state: "stopped" })), "pending");
});

test("the running stage's newest log line, without its timestamp", async () => {
  const { stageTicker } = await import("../src/lib/wizard.ts");
  const steps = [{ ...step("provision", "running"), logTail: ["[12:00:01] apt-get update", "[12:00:09] Setting up git (1:2.43)", ""] }];
  assert.equal(stageTicker(steps, def("provision")), "Setting up git (1:2.43)");
  assert.equal(stageTicker([], def("provision")), null);
});

test("install copy is platform-neutral and never tells people to install tools", async () => {
  const { explainFailure } = await import("../src/lib/wizard.ts");
  const texts = WIZARD_STEPS.flatMap((d) => [d.title, d.description, d.helper]);
  for (const text of ["no route to host", "not enough free disk", "download failed after 5", "qemu missing", "temporary failure resolving x", "qemu archive"]) {
    const { title, advice } = explainFailure(text);
    texts.push(title, advice);
  }
  for (const text of texts) {
    assert.doesNotMatch(text, /\bMac\b|ARM64|install qemu|cabind/i, text);
  }
});
