// Build a static Linux binary for the Ubuntu guest. cargo-zigbuild provides
// the cross linker when installed. Otherwise use Cargo's configured linker.
import { spawnSync } from "node:child_process";
import process from "node:process";

// The guest runs the host architecture wherever hardware acceleration can
// (macOS keeps its Apple Silicon product target).
const target = process.platform === "darwin" || process.arch === "arm64"
  ? "aarch64-unknown-linux-musl"
  : "x86_64-unknown-linux-musl";
const args = ["--release", "--bin", "cabind", "--target", target];

function has(program) {
  const check = spawnSync(program, ["--version"], {
    stdio: "ignore",
    shell: process.platform === "win32",
  });
  return check.status === 0;
}

const zigbuild = has("cargo-zigbuild");
const argv = zigbuild
  ? ["zigbuild", ...args]
  : ["build", ...args];
const result = spawnSync("cargo", argv, {
  stdio: "inherit",
  cwd: new URL("../core", import.meta.url),
  shell: process.platform === "win32",
});
process.exit(result.status ?? 1);
