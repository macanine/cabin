import assert from "node:assert/strict";
import test from "node:test";
import {
  chosenOs, defaultOptions, diskGb, formProblem, hostnameProblem, installSummary, matchingPreset, mirrorHost, optionsForHost,
  pickPreset, presetFitsHost, sanitizeUsername, usernameProblem,
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

test("the size preset follows host memory, CPUs and disk and never exceeds half the memory", () => {
  const gib = (n) => n * 1024;
  for (const [mem, cpus, key] of [[4, 4, "light"], [8, 8, "balanced"], [12, 8, "balanced"], [16, 8, "roomy"], [64, 4, "balanced"], [8, 2, "light"]]) {
    assert.equal(pickPreset(gib(mem), cpus).key, key, `${mem} GiB, ${cpus} CPUs`);
  }
  assert.equal(pickPreset(0, 8).key, "balanced");
  for (const mem of [2, 3, 4, 6, 8, 12, 16, 32]) assert.ok(pickPreset(gib(mem), 16).memoryMb <= Math.max(gib(mem) / 2, 2048), String(mem));
  assert.equal(pickPreset(16384, 10, 39).key, "balanced");
  assert.equal(pickPreset(16384, 10, 19).key, "light");
  const host = { memoryMb: 3584, cpus: 1, maxDiskGb: 10 };
  const custom = pickPreset(host.memoryMb, host.cpus, host.maxDiskGb);
  assert.deepEqual([custom.key, custom.cpus, custom.memoryMb, custom.diskGb], ["custom", 1, 1536, 10]);
  assert.equal(presetFitsHost(custom, host), true);
  assert.equal(presetFitsHost(pickPreset(8192, 4), host), false);
  const tooSmall = pickPreset(2048, 1, 6);
  assert.deepEqual([tooSmall.memoryMb, tooSmall.diskGb], [1024, 8]);
});

test("host account names become valid Linux usernames", () => {
  for (const [raw, name] of [["Nic Smith", "nic_smith"], ["CORP\\Jane.Doe", "jane_doe"], ["jane@corp.com", "jane"], ["1234", "u1234"], ["root", null], ["***", null]]) {
    assert.equal(sanitizeUsername(raw), name, raw);
  }
  assert.equal(sanitizeUsername("a".repeat(40))?.length, 32);
  for (const raw of ["x", "_a", "9lives", "Ünï"]) {
    const name = sanitizeUsername(raw);
    assert.ok(name === null || usernameProblem(name) === null, raw);
  }
});

test("first-run options come from the host", () => {
  const options = optionsForHost({ memoryMb: 16384, cpus: 10, maxDiskGb: 212, username: "Nic", deviceProfileSupported: true, platform: "macos" });
  assert.equal(matchingPreset(options)?.key, "roomy");
  assert.equal(options.username, "nic");
  assert.equal(formProblem(options), null);
  assert.equal(optionsForHost(null).username, null);
});
