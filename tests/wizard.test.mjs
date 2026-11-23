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

const failures = [
  ["preflight failed: not enough free disk: 1.9 GiB", /disk space/, /Free up space/],
  ['step "image" failed: not enough free space to download to image.img.part', /disk space/, /Free up space/],
  ["QEMU could not allocate memory for the VM. Close other apps or reduce the VM's memory.", /enough memory/, /smaller/],
  ["another vmctl operation is in progress (lock owner: pid=42)", /already running/, /Wait/],
  ["download cancelled", /cancelled/, /retry/],
  ["could not find a Ubuntu 24.04 LTS image: TLS verification or handshake failed", /secure download/, /date and time/],
  ["download received an HTML page instead of the file from https://mirror.test; sign in to the network if it requires a captive portal", /sign-in page/, /sign in/],
  ["download failed after 5 attempts: download DNS lookup failed: Dns Failed", /find the download server/, /DNS/],
  ["download failed after 5 attempts: download stalled (no bytes received for 30 seconds)", /stopped receiving data/, /saved partial/],
  ["HTTP 407 Proxy Authentication Required", /proxy could not connect/, /credentials/],
  ["--user must be a valid lowercase Linux username (max 32 chars, excluding root)", /username and hostname/, /lowercase/],
  ["--hostname must be a valid RFC1123 hostname (lowercase, max 63 chars)", /username and hostname/, /lowercase/],
  ['step "seed" failed: invalid SSH public key: expected one OpenSSH ed25519 public key', /public key could not be prepared/, /fresh key/],
  ["could not check free disk space at C:/Users/test/AppData: Access is denied", /size could not be checked/, /valid disk size/],
  ['step "disk" failed: qemu-img convert failed (exit 1): Permission denied', /opened for writing/, /data folder/],
  ["qemu-img convert failed (exit 1): The process cannot access the file because it is being used by another process", /opened for writing/, /other program/],
  ["qemu-img resize failed (exit 1): unexpected error", /disk could not be prepared/, /disk step log/],
  ["could not find a Ubuntu 24.04 LTS image: no valid image checksum in https://mirror.test/SHA256SUMS", /checksum list/, /mirror finishes syncing/],
  ['step "image" failed: could not find a Ubuntu 26.04 LTS image: HTTP 404', /system image/, /Ubuntu/],
  ["no recent Ubuntu 24.04 LTS build lists an image for this architecture", /system image/, /Ubuntu/],
  ["embedded image checksum mismatch: abcd", /included Ubuntu image/, /Reinstall/],
  ["checksum mismatch for https://mirror.test: expected digest, got different-digest", /did not match its checksum/, /pinned image/],
  ['step "qemu" failed: could not unpack the QEMU archive: invalid header', /engine could not be prepared/, /reinstall/],
  ["Could not launch QEMU using qemu-system-x86_64.exe: Operation did not complete successfully (os error 225)", /QEMU could not run/, /antivirus/],
  ["Cabin's bundled QEMU is missing or damaged. Reinstall Cabin.", /bundled QEMU is missing/, /quarantined/],
  ["required tool missing: ssh-keygen", /SSH tools/, /Reinstall Cabin/],
  ["download Content-Length does not match Content-Range from https://mirror.test", /could not be downloaded/, /connection/],
  ["download server rejected the saved partial range from https://mirror.test", /could not be downloaded/, /connection/],
  ["download failed after 5 attempts: could not connect: dns", /could not be downloaded/, /proxy/],
  ["HTTP 404 Not Found for https://x", /could not be downloaded/, /mirrors/],
  ["could not download https://cloud-images.ubuntu.com/x.img: connection reset by peer", /could not be downloaded/, /connection/],
  ["E: Failed to fetch … Temporary failure resolving 'ports.ubuntu.com'", /internet/, /network settings/],
  ["this machine runs debian-12, and Debian is no longer supported", /needs Ubuntu/, /Back up/],
  ["--cpus must be between 1 and 64", /Check the machine settings/, /Advanced/],
  ["The device profile is currently built for ARM machines; disable it to install on this host", /device profile is unavailable/, /Turn off/],
  ["The machine is already installed or has files to recover", /needs recovery/, /Verify or repair/],
  ["Cannot identify the recorded QEMU process (PID 42). Stop it before starting or resetting the machine.", /previous machine process/, /restart/],
  ["A VM from an earlier version of Cabin is still running with TCP control sockets.", /previous version/, /Stop/],
  ["VM socket path is too long (110 bytes, maximum 103): path. Use a shorter Cabin data directory.", /data path/, /shorter/],
  ["Unexpected file at VM socket path x; move it before starting Cabin", /control socket/, /move/],
  ["QEMU could not bind the SSH forwarding port. Free that port and retry.", /port could not be opened/, /Close/],
  ["Another process has the VM disk open.", /process has the VM disk/, /close the program/],
  ["QEMU could not use hardware virtualization (HVF).", /Apple’s virtualization/, /Applications/],
  ["WHvCreatePartition failed with 0x80070005", /Windows hardware acceleration/, /firmware/],
  ["I/O error: QEMU control request timed out", /control connection/, /copy diagnostics/],
  ["QEMU exited during startup (exit status: 1): QEMU produced no stderr.", /engine stopped/, /console/],
  ["WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!", /host key changed/, /Verify or repair/],
  ["SSH did not become ready within 180s: Connection refused", /ready for SSH/, /software emulation/],
  ["Timed out deploying cabind in the VM", /service could not start/, /start log/],
  ["Guest cabind binary not found at /other/cabind.", /VM service is missing/, /fresh copy/],
  ["preflight failed: cannot write VMCTL_HOME x: Permission denied", /access a required file/, /folder/],
  ["timed out after 900s waiting for cloud-init", /took too long/, /guest console/],
  ["SSH authentication failed", /log in.*SSH/, /Verify|verify/],
  ["dev@localhost: Permission denied (publickey).", /log in.*SSH/, /guest console/],
  ["cloud-init did not finish cleanly (rc=2)", /initial setup/, /package/],
  ["qemu-guest-agent package is not installed", /initial setup/, /package/],
  ["JSON error: expected value at line 1 column 1", /machine’s configuration/, /verify/],
  ["preflight failed: unsupported host architecture: x86", /setup checks/, /readiness/],
  ["preflight failed: TCP port 12233 on 127.0.0.1 is already in use; pass --ssh-port with a free port", /Port 12233 is already in use/, /Close/],
  ["SSH port 12233 on 127.0.0.1 is already in use. Free that port or choose a different SSH port.", /Port 12233 is already in use/, /Close/],
  ['step "qemu" failed: incomplete binaries', /engine could not be prepared/, /engine step log/],
  ['step "image" failed: Input/output error', /image could not be prepared/, /image step log/],
  ['step "disk" failed: I/O error: file already exists', /disk could not be prepared/, /disk step log/],
  ['step "seed" failed: the seed ISO is unreadable or its volumes disagree', /initial configuration/, /seed step log/],
  ['step "provision" failed: seed ISO is missing', /initial setup/, /first-boot log/],
  ['step "desktop" failed: desktop install failed (rc=1)', /desktop could not be set up/, /desktop step log/],
  ['step "browser" failed: browser fingerprint installation failed (rc=1)', /browser could not be set up/, /browser step log/],
  ['step "device-network" failed: set device.network.ssh_host', /device network/, /network settings/],
  ['step "device-guest" failed: profile verification failed', /device profile/, /Turn off|turn off/],
  ['step "agentd" failed: connection closed', /service could not start/, /start log/],
  ["something odd", /step failed/, /Details/],
];

for (const [error, title, advice] of failures) {
  test(`explains ${error}`, () => {
    const help = explainFailure(error);
    assert.match(help.title, title);
    assert.match(help.advice, advice);
  });
}

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
