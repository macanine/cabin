// Compile-check the Windows host build from macOS or Linux, in about a minute
// instead of a CI round trip. It uses cargo-zigbuild for the C parts
// (ring, aws-lc) and the windows-gnu target. MSVC-only differences remain
// CI's job. The check must be warning-free: Windows warnings are where
// platform drift starts.
//
//   npm run check:win            vm and src-tauri libraries and tests
//   npm run check:win -- vm      one crate
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const root = path.resolve(new URL("..", import.meta.url).pathname);
const target = "x86_64-pc-windows-gnu";
const env = { ...process.env, PATH: `${process.env.HOME}/.cargo/bin:${process.env.PATH}` };
const run = (cmd, args, cwd = root, extra = {}) =>
  spawnSync(cmd, args, { cwd, env: { ...env, ...extra }, stdio: "inherit" }).status === 0;

if (spawnSync("cargo", ["zigbuild", "--help"], { env, stdio: "ignore" }).status !== 0) {
  console.error("check:win needs zig and cargo-zigbuild: brew install zig && cargo install cargo-zigbuild");
  process.exit(1);
}
run("rustup", ["target", "add", target]);

const crates = process.argv.slice(2).length ? process.argv.slice(2) : ["vm", "src-tauri"];
const checks = { vm: ["--lib", "--tests"], "src-tauri": ["--lib", "--tests"] };
const appEnv = {};
if (crates.includes("src-tauri")) {
  const vendor = path.join(root, "build/vendor/windows-x64/manifest.json");
  if (!fs.existsSync(vendor) && !run("node", ["scripts/fetch-vendor.mjs", "--platform", "windows-x64"])) process.exit(1);
  if (spawnSync("x86_64-w64-mingw32-windres", ["--version"], { env, stdio: "ignore" }).status !== 0) {
    console.error("check:win needs windres for the app's Windows resources: brew install mingw-w64");
    process.exit(1);
  }
  // This build checks the host code. Drop the guest resource through Tauri's
  // config merge so a missing guest build never requires a fake binary.
  const config = JSON.parse(env.TAURI_CONFIG ?? "{}");
  (config.bundle ??= {}).resources ??= {};
  config.bundle.resources["../core/target/x86_64-unknown-linux-musl/release/cabind"] = null;
  appEnv.TAURI_CONFIG = JSON.stringify(config);
}
let ok = true;
for (const crate of crates) {
  console.log(`\n== ${crate} (${target})`);
  const args = ["zigbuild", "--target", target, ...(checks[crate] ?? ["--lib"])];
  ok = run("cargo", args, path.join(root, crate), {
    RUSTFLAGS: "-D warnings",
    ...(crate === "src-tauri" ? appEnv : {}),
  }) && ok;
}
process.exit(ok ? 0 : 1);
