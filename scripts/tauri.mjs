// One entry point for running and building the desktop app on macOS and
// Windows (and Linux development hosts).
//
//   npm run app:dev                    tauri dev on this host
//   npm run app:build [-- --debug]     tauri build on this host
//   npm run tauri:mac -- <args>        same wrapper, refuses to run off macOS
//   npm run tauri:win -- <args>        same wrapper, refuses to run off Windows
//
// Tauri merges src-tauri/tauri.<platform>.conf.json by itself, so the wrapper
// never passes --config. It only prepares the environment:
//
// - puts Cargo on PATH even when the shell did not source the user's profile;
// - keeps Rust artifacts in a per-host cache directory (the tree may be shared
//   with a Linux VM or another OS), unless CARGO_TARGET_DIR is already set;
// - checks the guest daemon toolchain before the long host compile starts,
//   because beforeDevCommand/beforeBuildCommand cross-build `cabind` first.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const isWin = process.platform === "win32";

const HOSTS = {
  darwin: {
    name: "macos",
    guest: "aarch64-unknown-linux-musl",
    targetDir: () => path.join(os.homedir(), "Library", "Caches", "cabin", "target-macos"),
  },
  win32: {
    name: "windows",
    guest: "x86_64-unknown-linux-musl",
    targetDir: () =>
      path.join(process.env.LOCALAPPDATA || path.join(os.homedir(), "AppData", "Local"), "cabin", "target-windows"),
  },
  linux: {
    name: "linux",
    guest: process.arch === "arm64" ? "aarch64-unknown-linux-musl" : "x86_64-unknown-linux-musl",
    // Linux keeps Cargo's default (src-tauri/target).
    targetDir: () => null,
  },
};

function fail(message) {
  console.error(`tauri: ${message}`);
  process.exit(1);
}

const args = process.argv.slice(2);
const host = HOSTS[process.platform] ?? fail(`unsupported host platform ${process.platform}`);

// `--host macos|windows` (used by the tauri:mac / tauri:win aliases).
if (args[0] === "--host") {
  const expected = args[1];
  args.splice(0, 2);
  if (expected !== host.name) {
    fail(`this command is for ${expected}, but this host is ${host.name}. Use npm run app:dev or npm run app:build.`);
  }
}

const env = { ...process.env };
const pathKey = Object.keys(env).find((key) => key.toUpperCase() === "PATH") ?? "PATH";
const prependPath = (dir) => {
  env[pathKey] = `${dir}${path.delimiter}${env[pathKey] ?? ""}`;
};
const run = (cmd, cmdArgs) =>
  spawnSync(cmd, cmdArgs, { env, encoding: "utf8", shell: isWin, stdio: ["ignore", "pipe", "pipe"] });
const has = (cmd) => run(cmd, ["--version"]).status === 0;

// Cargo-installed tools such as cargo-zigbuild live here.
const cargoBin = path.join(env.CARGO_HOME || path.join(os.homedir(), ".cargo"), "bin");
if (fs.existsSync(cargoBin)) prependPath(cargoBin);
if (!has("cargo")) {
  const which = has("rustup") ? run("rustup", ["which", "cargo"]) : null;
  if (!which || which.status !== 0) fail("Rust is required. Install rustup and a stable toolchain first.");
  prependPath(path.dirname(which.stdout.trim()));
}

const targetDir = host.targetDir();
if (targetDir && !env.CARGO_TARGET_DIR) env.CARGO_TARGET_DIR = targetDir;

const tauriCli = path.join(root, "node_modules", "@tauri-apps", "cli", "tauri.js");
if (!fs.existsSync(tauriCli)) {
  fail(`node_modules is missing the Tauri CLI. Run npm ci${host.name === "macos" ? " (or npm run setup:mac for a shared checkout)" : ""}.`);
}

// dev and build cross-compile the guest daemon first; report every missing
// piece at once instead of after the frontend and vendor steps.
const command = args.find((arg) => !arg.startsWith("-"));
if ((command === "dev" || command === "build") && !env.CABIN_PREBUILT_CABIND) {
  const missing = [];
  const installed = run("rustup", ["target", "list", "--installed"]);
  if (installed.status === 0 && !installed.stdout.split(/\s+/).includes(host.guest)) {
    missing.push(`rustup target add ${host.guest}`);
  }
  if (host.name !== "linux") {
    if (run("zig", ["version"]).status !== 0) missing.push(host.name === "macos" ? "brew install zig" : "winget install zig.zig");
    if (run("cargo", ["zigbuild", "--help"]).status !== 0) missing.push("cargo install cargo-zigbuild");
  }
  if (missing.length) {
    fail(`the guest daemon (${host.guest}) needs:\n  ${missing.join("\n  ")}`);
  }
}

console.log(`tauri: ${host.name} host, guest ${host.guest}, CARGO_TARGET_DIR=${env.CARGO_TARGET_DIR ?? "(default)"}`);
const result = spawnSync(process.execPath, [tauriCli, ...args], { cwd: root, env, stdio: "inherit" });
if (result.error) fail(result.error.message);
process.exit(result.status ?? 1);
