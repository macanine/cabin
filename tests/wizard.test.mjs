import assert from "node:assert/strict";
import test from "node:test";
import { currentWizardStep, WIZARD_STEPS, wizardState } from "../src/lib/wizard.ts";

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

test("failures are explained in plain language", async () => {
  const { explainFailure, overallProgress } = await import("../src/lib/wizard.ts");
  const { formatDuration } = await import("../src/lib/format.ts");
  assert.match(explainFailure("E: Failed to fetch … Temporary failure resolving 'ports.ubuntu.com'").title, /internet/);
  assert.match(explainFailure("preflight failed: not enough free disk: 1.9 GiB").title, /disk space/);
  assert.match(explainFailure("preflight failed: TCP port 12233 on 127.0.0.1 is already in use; pass --ssh-port with a free port").title, /Port 12233 is already in use/);
  assert.match(explainFailure("something odd").title, /step failed/);
  assert.match(explainFailure('step "image" failed: could not find a Ubuntu 26.04 LTS image: HTTP 404').title, /system image/);
  assert.match(explainFailure('step "qemu" failed: could not unpack the QEMU archive: x').title, /virtual machine engine/);
  assert.match(explainFailure("qemu not found").title, /bundled QEMU is missing/);
  assert.match(explainFailure("download failed after 5 attempts: could not connect: dns").title, /could not be downloaded/);
  assert.match(explainFailure("HTTP 404 Not Found for https://x").title, /could not be downloaded/);
  assert.match(explainFailure("could not download https://cloud-images.ubuntu.com/x.img: connection reset by peer").title, /could not be downloaded/);
  assert.match(explainFailure('step "device-network" failed: set device.network.ssh_host').title, /device network/);
  assert.match(explainFailure("SSH authentication failed").title, /log in.*SSH/);
  assert.equal(formatDuration(75_000), "1m 15s");
  assert.equal(overallProgress(["done", "cached", "running", "pending", "pending", "pending"]), 2.5 / 6);
  assert.equal(overallProgress(["running", "pending", "pending", "pending", "pending", "pending"], [0.6]), 0.6 / 6);
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

test("install input failures describe the cause and a recovery action", async () => {
  const { explainFailure } = await import("../src/lib/wizard.ts");
  for (const [error, title, advice] of [
    ['step "image" failed: not enough free space to download to image.img.part', /disk space/, /Free up space/],
    ['step "disk" failed: not enough free disk space at instances: 1 GiB available', /disk space/, /Free up space/],
    ["another vmctl operation is in progress (lock owner: pid=42)", /already running/, /Wait/],
    ["could not find a Ubuntu 24.04 LTS image: could not download index: TLS verification or handshake failed", /secure download/, /date and time/],
    ["download received an HTML page instead of the file from https://mirror.test; sign in to the network if it requires a captive portal", /sign-in page/, /sign in/],
    ["download failed after 5 attempts: download DNS lookup failed: Dns Failed", /find the download server/, /DNS/],
    ["download failed after 5 attempts: download stalled (no bytes received for 30 seconds)", /stopped receiving data/, /saved partial/],
    ["download Content-Length does not match Content-Range from https://mirror.test", /could not be downloaded/, /connection/],
    ["download server rejected the saved partial range from https://mirror.test", /could not be downloaded/, /connection/],
    ["could not find a Ubuntu 24.04 LTS image: no valid image checksum in https://mirror.test/SHA256SUMS", /system image/, /Ubuntu/],
    ["checksum mismatch for https://mirror.test: expected digest, got different-digest", /checksum/, /pinned image/],
    ['step "disk" failed: qemu-img convert failed (exit 1): Permission denied', /opened for writing/, /data folder/],
    ["qemu-img convert failed (exit 1): The process cannot access the file because it is being used by another process", /opened for writing/, /other program/],
    ["qemu-img resize failed (exit 1): unexpected error", /disk could not be prepared/, /disk step log/],
    ["--user must be a valid lowercase Linux username (max 32 chars)", /username and hostname/, /lowercase/],
    ["--hostname must be a valid RFC1123 hostname (lowercase, max 63 chars)", /username and hostname/, /lowercase/],
    ['step "seed" failed: invalid SSH public key: expected one OpenSSH ed25519 public key', /public key could not be prepared/, /fresh key/],
    ['step "seed" failed: the seed ISO is unreadable or its volumes disagree', /initial configuration/, /seed step log/],
  ]) {
    const help = explainFailure(error);
    assert.match(help.title, title, error);
    assert.match(help.advice, advice, error);
  }
});
