import assert from "node:assert/strict";
import test from "node:test";
import {
  chosenOs, defaultOptions, diskGb, formProblem, hostnameProblem, installSummary, matchingPreset, mirrorHost, optionsForHost,
  pickPreset, presetFitsHost, readinessView, sanitizeUsername, setupSummary, usernameProblem,
} from "../src/lib/installForm.ts";

test("username and hostname follow the VM's rules", () => {
  for (const ok of ["", "dev", "_svc", "a-b_9"]) assert.equal(usernameProblem(ok), null, ok);
  for (const bad of ["Dev", "1dev", "root", "dev.user", "a".repeat(33)]) assert.ok(usernameProblem(bad), bad);
  for (const ok of ["", "agent", "agent-vm-2"]) assert.equal(hostnameProblem(ok), null, ok);
  for (const bad of ["-vm", "vm-", "my_vm", "VM", "a".repeat(64)]) assert.ok(hostnameProblem(bad), bad);
});

test("presets, disk sizes and the one form check", () => {
  const options = defaultOptions();
  assert.equal(matchingPreset(options)?.key, "balanced");
  assert.equal(matchingPreset({ ...options, cpus: 3 }), null);
  assert.equal(diskGb("20G"), 20);
  assert.equal(diskGb("20GB"), null);
  assert.equal(formProblem(options), null);
  assert.equal(formProblem({ ...options, username: "root" }), "root is reserved; choose a normal user name.");
  assert.ok(formProblem({ ...options, diskSize: "4G" }));
  assert.ok(formProblem({ ...options, hostname: "Bad", memoryMb: 512 }).includes("lowercase"));
});

test("the summary names the system and the size", () => {
  const choices = [
    { id: "ubuntu-24.04", name: "Ubuntu 24.04 LTS", testing: false, mirrors: [] },
    { id: "ubuntu-26.04", name: "Ubuntu 26.04 LTS", testing: false, mirrors: [] },
  ];
  const options = defaultOptions();
  assert.equal(chosenOs(options, choices).id, "ubuntu-24.04");
  assert.equal(chosenOs({ ...options, os: "ubuntu-26.04" }, choices).id, "ubuntu-26.04");
  assert.equal(installSummary(options, choices[0]), "Ubuntu 24.04 LTS · 4 CPU · 4 GB memory · 20 GB disk");
  assert.equal(mirrorHost("https://ftp.kaist.ac.kr/ubuntu-ports"), "ftp.kaist.ac.kr");
  assert.equal(mirrorHost(null), null);
});

test("the size preset follows host memory and CPUs and never exceeds half the memory", () => {
  const gib = (n) => n * 1024;
  assert.equal(pickPreset(gib(4), 4).key, "light");
  assert.equal(pickPreset(gib(7.8), 8).key, "balanced");
  assert.equal(pickPreset(gib(8), 8).key, "balanced");
  assert.equal(pickPreset(gib(15.8), 8).key, "roomy");
  assert.equal(pickPreset(gib(12), 8).key, "balanced");
  assert.equal(pickPreset(gib(16), 8).key, "roomy");
  assert.equal(pickPreset(gib(64), 16).key, "roomy");
  assert.equal(pickPreset(gib(64), 4).key, "balanced");
  assert.equal(pickPreset(gib(8), 2).key, "light");
  assert.equal(pickPreset(0, 8).key, "balanced");
  for (const mem of [2, 3, 4, 6, 8, 12, 16, 32]) assert.ok(pickPreset(gib(mem), 16).memoryMb <= Math.max(gib(mem) / 2, 2048), String(mem));
});

test("host account names become valid Linux usernames", () => {
  assert.equal(sanitizeUsername("nic"), "nic");
  assert.equal(sanitizeUsername("Nic Smith"), "nic_smith");
  assert.equal(sanitizeUsername("CORP\\Jane.Doe"), "jane_doe");
  assert.equal(sanitizeUsername("jane@corp.com"), "jane");
  assert.equal(sanitizeUsername("1234"), "u1234");
  assert.equal(sanitizeUsername("root"), null);
  assert.equal(sanitizeUsername(""), null);
  assert.equal(sanitizeUsername("***"), null);
  assert.equal(sanitizeUsername("a".repeat(40))?.length, 32);
  for (const raw of ["Nic Smith", "x", "_a", "9lives", "Ünï"]) {
    const name = sanitizeUsername(raw);
    assert.ok(name === null || usernameProblem(name) === null, raw);
  }
});

test("first-run options come from the host", () => {
  const host = { memoryMb: 16384, cpus: 10, maxDiskGb: 212, username: "Nic", deviceProfileSupported: true, platform: "macos" };
  const options = optionsForHost(host);
  assert.equal(matchingPreset(options)?.key, "roomy");
  assert.equal(options.username, "nic");
  assert.equal(formProblem(options), null);
  assert.equal(optionsForHost(null).username, null);
  assert.equal(setupSummary(options, { id: "u", name: "Ubuntu 24.04 LTS", testing: false, mirrors: [] }, "dev"), "Ubuntu 24.04 LTS · Roomy (8 CPUs, 8 GB, 40 GB) · user nic");
  assert.match(setupSummary({ ...options, cpus: 3, username: null }, null, "dev"), /Custom \(3 CPUs.*user dev$/);
});

test("presets respect available disk, CPU count and the host's memory reserve", () => {
  assert.equal(pickPreset(16384, 10, 39).key, "balanced");
  assert.equal(pickPreset(16384, 10, 19).key, "light");
  const host = { memoryMb: 3584, cpus: 1, maxDiskGb: 10 };
  const custom = pickPreset(host.memoryMb, host.cpus, host.maxDiskGb);
  assert.equal(custom.key, "custom");
  assert.deepEqual([custom.cpus, custom.memoryMb, custom.diskGb], [1, 1536, 10]);
  assert.equal(presetFitsHost(custom, host), true);
  assert.equal(presetFitsHost(pickPreset(8192, 4), host), false);
  // Below the supported minimum, the form keeps valid input for the readiness blocker.
  const tooSmall = pickPreset(2048, 1, 6);
  assert.deepEqual([tooSmall.memoryMb, tooSmall.diskGb], [1024, 8]);
  assert.equal(presetFitsHost(tooSmall, { memoryMb: 2048, cpus: 1, maxDiskGb: 6 }), false);
});

test("readiness splits into blockers, warnings and passes", () => {
  const item = (id, ok, optional) => ({ id, label: id, ok, optional, detail: "", fix: null });
  const view = readinessView([item("accel", false, true), item("ssh", false, false), item("disk", true, false), item("memory", false, true)]);
  assert.deepEqual(view.blocking.map((i) => i.id), ["ssh"]);
  assert.deepEqual(view.warnings.map((i) => i.id), ["accel", "memory"]);
  assert.deepEqual(view.passed.map((i) => i.id), ["disk"]);
});
