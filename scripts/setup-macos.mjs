import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

if (process.platform !== "darwin" || !["arm64", "x64"].includes(process.arch)) {
  throw new Error("setup:mac requires macOS on arm64 or x64.");
}

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const modules = join(root, "node_modules");
if (!existsSync(modules)) {
  execFileSync("npm", ["ci", "--no-audit", "--no-fund"], { cwd: root, stdio: "inherit" });
  process.exit(0);
}

const lock = JSON.parse(readFileSync(join(root, "package-lock.json"), "utf8"));
const nativePackages = [
  `@rolldown/binding-darwin-${process.arch}`,
  `@tauri-apps/cli-darwin-${process.arch}`,
  `lightningcss-darwin-${process.arch}`,
  "fsevents",
];
const packageVersion = (base, name) => {
  const manifest = join(base, name, "package.json");
  return existsSync(manifest) ? JSON.parse(readFileSync(manifest, "utf8")).version : null;
};

for (const name of ["rolldown", "@tauri-apps/cli", "lightningcss", ...nativePackages]) {
  const expected = lock.packages[`node_modules/${name}`]?.version;
  const installed = packageVersion(modules, name);
  if (!expected || (installed && installed !== expected)) {
    throw new Error(`${name} does not match package-lock.json; use a separate checkout and run npm ci.`);
  }
  if (!installed && !nativePackages.includes(name)) {
    throw new Error(`node_modules is incomplete (${name} is missing); use a separate checkout and run npm ci.`);
  }
}

const missing = nativePackages.filter((name) => !packageVersion(modules, name));
if (missing.length === 0) {
  console.log("macOS native dependencies are already installed.");
  process.exit(0);
}

// Install into a temporary directory instead of pruning the Linux packages
// in a shared node_modules. Copy only the missing, lockfile-matched binaries.
const staging = mkdtempSync(join(tmpdir(), "cabin-macos-deps-"));
for (const file of ["package.json", "package-lock.json"]) {
  cpSync(join(root, file), join(staging, file));
}
execFileSync("npm", ["ci", "--no-audit", "--no-fund"], { cwd: staging, stdio: "inherit" });
for (const name of missing) {
  const source = join(staging, "node_modules", name);
  const target = join(modules, name);
  if (!existsSync(source)) throw new Error(`npm did not install ${name} for this Mac.`);
  mkdirSync(dirname(target), { recursive: true });
  cpSync(source, target, { recursive: true, force: false, errorOnExist: true });
  console.log(`Installed ${name}`);
}
