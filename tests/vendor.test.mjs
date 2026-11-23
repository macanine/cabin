import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs/promises";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { download, signMacQemu } from "../scripts/fetch-vendor.mjs";

test("vendor download tries every URL with one digest and only caches verified bytes", async (t) => {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), "cabin-vendor-"));
  t.after(() => fs.rm(dir, { recursive: true, force: true }));
  const payload = "the pinned installer";
  const seen = [];
  const server = http.createServer((request, response) => {
    seen.push(request.url);
    if (request.url === "/rotated") response.writeHead(404).end();
    else response.end(request.url === "/mirror" ? payload : "bad mirror data");
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const base = `http://127.0.0.1:${server.address().port}`;
  const sha256 = createHash("sha256").update(payload).digest("hex");
  const first = [base + "/rotated", base + "/corrupt"];
  const file = path.join(dir, "qemu.exe");

  await assert.rejects(download({ urls: first, sha256 }, file), /HTTP 404[\s\S]*sha256 mismatch/);
  assert.deepEqual(await fs.readdir(dir), []);
  await download({ urls: [...first, base + "/mirror"], sha256 }, file);
  assert.deepEqual(seen, ["/rotated", "/corrupt", "/rotated", "/corrupt", "/mirror"]);
  assert.equal(await fs.readFile(file, "utf8"), payload);
  await download({ urls: first, sha256 }, file);
  assert.equal(seen.length, 5);
});

test("ad-hoc QEMU signing strips quarantine, embeds HVF, and leaves dylibs loadable", { skip: process.platform !== "darwin" }, async (t) => {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), "cabin-sign-"));
  t.after(() => fs.rm(dir, { recursive: true, force: true }));
  const run = (cmd, args) => {
    const result = spawnSync(cmd, args, { encoding: "utf8" });
    assert.equal(result.status, 0, `${cmd}: ${result.stderr}`);
    return result.stdout;
  };
  await fs.mkdir(path.join(dir, "bin"));
  await fs.mkdir(path.join(dir, "lib"));
  const dylib = path.join(dir, "lib/libprobe.dylib");
  const system = path.join(dir, "bin/qemu-system-aarch64");
  const img = path.join(dir, "bin/qemu-img");
  const librarySource = path.join(dir, "lib.c");
  const binarySource = path.join(dir, "main.c");
  await fs.writeFile(librarySource, "int check(void) { return 0; }\n");
  await fs.writeFile(binarySource, "int check(void); int main(void) { return check(); }\n");
  run("cc", ["-dynamiclib", "-install_name", "@executable_path/../lib/libprobe.dylib", librarySource, "-o", dylib]);
  run("cc", [binarySource, dylib, "-o", system]);
  await fs.copyFile(system, img);
  await fs.writeFile(path.join(dir, "entitlements.plist"),
    '<?xml version="1.0"?><plist version="1.0"><dict><key>com.apple.security.hypervisor</key><true/></dict></plist>');
  run("/usr/bin/xattr", ["-w", "com.apple.quarantine", "0081;00000000;Cabin test;", system]);
  signMacQemu(dir);

  for (const binary of [system, img]) {
    run("/usr/bin/codesign", ["--verify", "--strict", binary]);
    assert.match(run("/usr/bin/codesign", ["-d", "--entitlements", "-", "--xml", binary]), /com.apple.security.hypervisor<\/key>\s*<true\s*\/>/);
    assert.doesNotMatch(run("/usr/bin/xattr", [binary]), /quarantine/);
    run(binary, []);
  }
});
