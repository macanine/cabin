import assert from "node:assert/strict";
import test from "node:test";
import {
  chosenOs, defaultOptions, diskGb, formProblem, hostnameProblem, installSummary, matchingPreset, mirrorHost, usernameProblem,
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
