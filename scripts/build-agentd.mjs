// Build a static Linux binary for the Ubuntu guest. cargo-zigbuild provides
// the cross linker when installed. Otherwise use Cargo's configured linker.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import process from "node:process";
import { fileURLToPath } from "node:url";

// Guest triple, first match wins: `--target <triple>`, CABIN_GUEST_ARCH
// (aarch64|x86_64), TAURI_ENV_TARGET_TRIPLE, then the host. The macOS product
// always runs an arm64 guest, and so does any other arm64 host.
const GUEST = {
  aarch64: "aarch64-unknown-linux-musl",
  x86_64: "x86_64-unknown-linux-musl",
};

function fromArch(arch) {
  const guest = GUEST[arch];
  if (!guest) throw new Error(`Unsupported guest architecture: ${arch}`);
  return guest;
}

function fromTriple(triple) {
  if (triple.endsWith("-apple-darwin")) return GUEST.aarch64;
  if (triple.startsWith("x86_64-")) return GUEST.x86_64;
  if (triple.startsWith("aarch64-")) return GUEST.aarch64;
  throw new Error(`Unsupported target triple: ${triple}`);
}

function pickTarget(argv, env) {
  const flag = argv.indexOf("--target");
  if (flag >= 0) {
    if (!argv[flag + 1]) throw new Error("--target needs a triple");
    return argv[flag + 1];
  }
  if (env.CABIN_GUEST_ARCH) return fromArch(env.CABIN_GUEST_ARCH);
  if (env.TAURI_ENV_TARGET_TRIPLE) return fromTriple(env.TAURI_ENV_TARGET_TRIPLE);
  return process.platform === "darwin" || process.arch === "arm64"
    ? GUEST.aarch64
    : GUEST.x86_64;
}

const target = pickTarget(process.argv.slice(2), process.env);
// CI builds the guest daemon once on Linux and drops it at the Cargo output
// path. CABIN_PREBUILT_CABIND=1 then skips the rebuild for a nonempty file.
const output = new URL(`../core/target/${target}/release/cabind`, import.meta.url);
const prebuilt = process.env.CABIN_PREBUILT_CABIND && fs.statSync(output, { throwIfNoEntry: false });
if (prebuilt && prebuilt.isFile() && prebuilt.size > 0) {
  console.log(`build-agentd: using prebuilt ${target} cabind`);
  process.exit(0);
}
const args = ["--locked", "--release", "--bin", "cabind", "--target", target];

const zigbuild = spawnSync("cargo", ["zigbuild", "--help"], {
  stdio: "ignore",
  shell: process.platform === "win32",
}).status === 0;
const argv = zigbuild
  ? ["zigbuild", ...args]
  : ["build", ...args];
// The bundle resources (tauri.<platform>.conf.json) and the prebuilt check
// above read core/target, so pin it even when the host build uses a shared
// CARGO_TARGET_DIR (scripts/tauri.mjs sets one on macOS and Windows).
const result = spawnSync("cargo", argv, {
  stdio: "inherit",
  cwd: new URL("../core", import.meta.url),
  env: { ...process.env, CARGO_TARGET_DIR: fileURLToPath(new URL("../core/target", import.meta.url)) },
  shell: process.platform === "win32",
});
process.exit(result.status ?? 1);
