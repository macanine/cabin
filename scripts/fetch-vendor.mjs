// Download, verify, and unpack the binaries that release builds ship inside
// the app bundle (QEMU everywhere, OpenSSH on Windows).
//
// Usage: node scripts/fetch-vendor.mjs [--platform macos-arm64|windows-x64]
//
// The platform defaults to TAURI_ENV_PLATFORM/TAURI_ENV_ARCH when Tauri sets
// them, and otherwise to the current host. Hosts without a pin (Linux) skip
// quietly, because Linux development uses the distro QEMU.
//
// Input: scripts/vendor.json
//   { schema: 1, platforms: { <platform>: {
//       guestArch: "aarch64" | "x86_64",
//       qemu:    <pin>,
//       openssh?: <pin> } } }
//   pin = { version, license, format: "tar.gz" | "nsis" | "zip", url, sha256,
//           sourceUrl, upstreamSourceUrl?, extraFiles?: [{ name, url, sha256 }] }
//
// Cache: build/vendor-cache/<sha256-prefix>-<file name> (hash-verified on every use).
//
// Output: build/vendor/<platform>/
//   qemu/            pruned QEMU tree (macOS: bin/ lib/ share/qemu/;
//                    Windows: exes and DLLs in the root, share/ with the x86 firmware)
//   openssh/         Windows only: ssh.exe, ssh-keygen.exe and friends
//   licenses/        license texts and notices of every component
//   SOURCES.txt      versions and source locations (GPL compliance)
//   manifest.json    paths relative to build/vendor/<platform>/:
//     { platform, guestArch, qemu: { version, system, img, firmwareCode,
//       firmwareVars, firmwareDir }, ssh?: { version, ssh, sshKeygen } }
//   .stamp.json      pin hashes of the unpacked tree; a match skips the work
//
// Tauri bundles build/vendor/<platform>/ as the resource directory `vendor/`.
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const cacheDir = path.join(root, "build", "vendor-cache");
const outRoot = path.join(root, "build", "vendor");
const STAMP_VERSION = 1;

const WINDOWS_QEMU_KEEP_FILES = [
  "qemu-system-x86_64.exe",
  "qemu-img.exe",
  "COPYING",
  "COPYING.LIB",
  "README.rst",
  "VERSION",
];
// Files in share/ that the x86-64 q35 machine loads without -L.
const WINDOWS_SHARE_KEEP = [
  /^edk2-x86_64-code\.fd$/,
  /^edk2-i386-vars\.fd$/,
  /^edk2-licenses\.txt$/,
  /^bios(-256k|-microvm)?\.bin$/,
  /^vgabios.*\.bin$/,
  /^(efi|pxe)-.*\.rom$/,
  /^(kvmvapic|linuxboot_dma|multiboot_dma|pvh)\.bin$/,
  /^qboot\.rom$/,
];
const WINDOWS_SHARE_KEEP_DIRS = ["keymaps", "firmware"];
const OPENSSH_KEEP = new Set([
  "ssh.exe",
  "ssh-keygen.exe",
  "ssh-keyscan.exe",
  "ssh-add.exe",
  "scp.exe",
  "sftp.exe",
  "libcrypto.dll",
  "LICENSE.txt",
  "NOTICE.txt",
]);

function sha256File(file) {
  const hash = createHash("sha256");
  hash.update(fs.readFileSync(file));
  return hash.digest("hex");
}

function detectPlatform() {
  const os = process.env.TAURI_ENV_PLATFORM || process.platform;
  const arch = process.env.TAURI_ENV_ARCH || process.arch;
  const isMac = os === "darwin" || os === "macos";
  const isWin = os === "windows" || os === "win32";
  const isArm = arch === "aarch64" || arch === "arm64";
  const isX64 = arch === "x86_64" || arch === "x64";
  if (isMac && isArm) return "macos-arm64";
  if (isWin && isX64) return "windows-x64";
  return null;
}

async function download(pin, dest) {
  if (fs.existsSync(dest) && sha256File(dest) === pin.sha256) return;
  fs.rmSync(dest, { force: true });
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  console.log(`download ${pin.url}`);
  const response = await fetch(pin.url, { redirect: "follow" });
  if (!response.ok || !response.body) {
    throw new Error(`download failed (${response.status}): ${pin.url}`);
  }
  const part = `${dest}.part`;
  const hash = createHash("sha256");
  const out = fs.createWriteStream(part);
  for await (const chunk of response.body) {
    hash.update(chunk);
    if (!out.write(chunk)) await new Promise((resolve) => out.once("drain", resolve));
  }
  await new Promise((resolve, reject) => out.end((e) => (e ? reject(e) : resolve())));
  const actual = hash.digest("hex");
  if (actual !== pin.sha256) {
    fs.rmSync(part, { force: true });
    throw new Error(
      `sha256 mismatch for ${pin.url}\n  expected ${pin.sha256}\n  actual   ${actual}`,
    );
  }
  fs.renameSync(part, dest);
}

function cachePath(pin) {
  return path.join(cacheDir, `${pin.sha256.slice(0, 12)}-${path.basename(new URL(pin.url).pathname)}`);
}

// Extract with the 7-Zip WebAssembly build, which reads NSIS installers and
// ZIP files on every host. The archive's drive root is mounted into its
// virtual file system so absolute host paths work.
async function extract7z(archive, dest) {
  const { default: SevenZip } = await import("7z-wasm");
  const sevenZip = await SevenZip({ print() {}, printErr: (m) => console.error(m) });
  const mountRoot = path.parse(archive).root;
  sevenZip.FS.mkdir("/host");
  sevenZip.FS.mount(sevenZip.NODEFS, { root: mountRoot }, "/host");
  // The WebAssembly build sets mode 000 on some extracted entries.
  const chmod = sevenZip.FS.chmod;
  sevenZip.FS.chmod = (p, mode, noFollow) => {
    if (mode) chmod(p, mode, noFollow);
  };
  const virtual = (p) => `/host/${path.relative(mountRoot, p).split(path.sep).join("/")}`;
  fs.mkdirSync(dest, { recursive: true });
  const code = sevenZip.callMain(["x", "-y", `-o${virtual(dest)}`, virtual(archive)]);
  if (code) throw new Error(`7-Zip failed (${code}) on ${archive}`);
}

function extractTar(archive, dest) {
  fs.mkdirSync(dest, { recursive: true });
  const result = spawnSync("tar", ["-xzf", archive, "-C", dest, "--strip-components=1"], {
    stdio: "inherit",
  });
  if (result.status !== 0) throw new Error(`tar failed on ${archive}`);
}

function freshDir(dir) {
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
}

function copyFile(from, to) {
  fs.mkdirSync(path.dirname(to), { recursive: true });
  fs.copyFileSync(from, to);
}

function need(file) {
  if (!fs.existsSync(file)) throw new Error(`Expected file missing after extraction: ${file}`);
}

async function stageMacQemu(pin, work, dest) {
  const archive = cachePath(pin);
  await download(pin, archive);
  extractTar(archive, dest);
  for (const rel of ["bin/qemu-system-aarch64", "bin/qemu-img", "share/qemu/edk2-aarch64-code.fd", "share/qemu/edk2-aarch64-vars.fd"]) {
    need(path.join(dest, rel));
  }
  fs.chmodSync(path.join(dest, "bin/qemu-system-aarch64"), 0o755);
  fs.chmodSync(path.join(dest, "bin/qemu-img"), 0o755);
  return {
    version: pin.version,
    system: "qemu/bin/qemu-system-aarch64",
    img: "qemu/bin/qemu-img",
    firmwareCode: "qemu/share/qemu/edk2-aarch64-code.fd",
    firmwareVars: "qemu/share/qemu/edk2-aarch64-vars.fd",
    firmwareDir: "qemu/share/qemu",
  };
}

async function stageWindowsQemu(pin, work, dest) {
  const archive = cachePath(pin);
  await download(pin, archive);
  const raw = path.join(work, "qemu-raw");
  freshDir(raw);
  await extract7z(archive, raw);
  fs.mkdirSync(dest, { recursive: true });
  for (const name of fs.readdirSync(raw)) {
    const from = path.join(raw, name);
    if (!fs.statSync(from).isFile()) continue;
    if (name.toLowerCase().endsWith(".dll") || WINDOWS_QEMU_KEEP_FILES.includes(name)) {
      copyFile(from, path.join(dest, name));
    }
  }
  const share = path.join(raw, "share");
  for (const name of fs.readdirSync(share)) {
    const from = path.join(share, name);
    const stat = fs.statSync(from);
    if (stat.isFile() && WINDOWS_SHARE_KEEP.some((re) => re.test(name))) {
      copyFile(from, path.join(dest, "share", name));
    } else if (stat.isDirectory() && WINDOWS_SHARE_KEEP_DIRS.includes(name)) {
      fs.cpSync(from, path.join(dest, "share", name), { recursive: true });
    }
  }
  for (const rel of ["qemu-system-x86_64.exe", "qemu-img.exe", "share/edk2-x86_64-code.fd", "share/edk2-i386-vars.fd"]) {
    need(path.join(dest, rel));
  }
  const version = fs.existsSync(path.join(dest, "VERSION"))
    ? fs.readFileSync(path.join(dest, "VERSION"), "utf8").trim()
    : pin.version;
  return {
    version,
    system: "qemu/qemu-system-x86_64.exe",
    img: "qemu/qemu-img.exe",
    firmwareCode: "qemu/share/edk2-x86_64-code.fd",
    firmwareVars: "qemu/share/edk2-i386-vars.fd",
    firmwareDir: "qemu/share",
  };
}

async function stageOpenssh(pin, work, dest) {
  const archive = cachePath(pin);
  await download(pin, archive);
  const raw = path.join(work, "openssh-raw");
  freshDir(raw);
  await extract7z(archive, raw);
  // The zip holds one OpenSSH-Win64 folder.
  const inner = fs.existsSync(path.join(raw, "ssh.exe"))
    ? raw
    : path.join(raw, fs.readdirSync(raw).find((n) => fs.statSync(path.join(raw, n)).isDirectory()));
  fs.mkdirSync(dest, { recursive: true });
  for (const name of fs.readdirSync(inner)) {
    if (OPENSSH_KEEP.has(name)) copyFile(path.join(inner, name), path.join(dest, name));
  }
  need(path.join(dest, "ssh.exe"));
  need(path.join(dest, "ssh-keygen.exe"));
  return { version: pin.version, ssh: "openssh/ssh.exe", sshKeygen: "openssh/ssh-keygen.exe" };
}

async function stageExtras(pin, licenses, prefix) {
  for (const extra of pin.extraFiles ?? []) {
    const cached = cachePath({ ...extra, url: extra.url });
    await download(extra, cached);
    copyFile(cached, path.join(licenses, `${prefix}-${extra.name}`));
  }
}

function sourcesText(platform, spec) {
  const lines = [
    `Cabin vendored components for ${platform}`,
    "",
    "These programs are distributed under the licenses named below, unmodified",
    "except for the build noted. The source code for each is available from the",
    "listed locations. You may also request it from the Cabin maintainers.",
    "",
  ];
  for (const [name, pin] of [["QEMU", spec.qemu], ["OpenSSH", spec.openssh]]) {
    if (!pin) continue;
    lines.push(
      `${name} ${pin.version}`,
      `  License:  ${pin.license}`,
      `  Binary:   ${pin.url}`,
      `  SHA-256:  ${pin.sha256}`,
      `  Source:   ${pin.sourceUrl}`,
    );
    if (pin.upstreamSourceUrl) lines.push(`  Upstream: ${pin.upstreamSourceUrl}`);
    lines.push("");
  }
  if (platform === "macos-arm64") {
    lines.push(
      "The macOS QEMU build also ships shared libraries (GnuTLS, GLib, pixman,",
      "SPICE, virglrenderer and others) in qemu/lib under their own licenses. The",
      "build recipe and patches are in the Source repository above; BUILD-INFO.txt",
      "in qemu/ lists the exact inputs.",
      "",
    );
  } else {
    lines.push(
      "The Windows QEMU installer by Stefan Weil bundles MinGW runtime libraries",
      "(the DLLs next to qemu-system-x86_64.exe) under their own licenses.",
      "",
    );
  }
  return lines.join("\n");
}

async function main() {
  const args = process.argv.slice(2);
  const flag = args.indexOf("--platform");
  const explicit = flag >= 0 ? args[flag + 1] : null;
  const platform = explicit ?? detectPlatform();
  const config = JSON.parse(fs.readFileSync(path.join(root, "scripts", "vendor.json"), "utf8"));
  if (!platform) {
    console.log("vendor: no pinned binaries for this host; Linux uses the distro QEMU.");
    return;
  }
  const spec = config.platforms[platform];
  if (!spec) throw new Error(`Unknown platform ${platform}. Known: ${Object.keys(config.platforms).join(", ")}`);

  const outDir = path.join(outRoot, platform);
  const stampFile = path.join(outDir, ".stamp.json");
  const stamp = {
    stamp: STAMP_VERSION,
    qemu: spec.qemu.sha256,
    openssh: spec.openssh?.sha256 ?? null,
    extras: (spec.qemu.extraFiles ?? []).map((e) => e.sha256),
  };
  try {
    const existing = JSON.parse(fs.readFileSync(stampFile, "utf8"));
    if (JSON.stringify(existing) === JSON.stringify(stamp) && fs.existsSync(path.join(outDir, "manifest.json"))) {
      console.log(`vendor: ${platform} is up to date`);
      return;
    }
  } catch {
    // No usable stamp: rebuild.
  }

  const work = path.join(cacheDir, `work-${platform}`);
  const staging = path.join(outRoot, `.${platform}.staging`);
  freshDir(staging);
  freshDir(work);
  const licenses = path.join(staging, "licenses");
  fs.mkdirSync(licenses, { recursive: true });

  const stageQemu = platform === "macos-arm64" ? stageMacQemu : stageWindowsQemu;
  const qemu = await stageQemu(spec.qemu, work, path.join(staging, "qemu"));
  await stageExtras(spec.qemu, licenses, "qemu");
  for (const name of ["COPYING", "COPYING.LIB"]) {
    const file = path.join(staging, "qemu", name);
    if (fs.existsSync(file)) copyFile(file, path.join(licenses, `qemu-${name}`));
  }
  const manifest = { platform, guestArch: spec.guestArch, qemu };
  if (spec.openssh) {
    manifest.ssh = await stageOpenssh(spec.openssh, work, path.join(staging, "openssh"));
    for (const name of ["LICENSE.txt", "NOTICE.txt"]) {
      const file = path.join(staging, "openssh", name);
      if (fs.existsSync(file)) copyFile(file, path.join(licenses, `openssh-${name}`));
    }
  }
  fs.writeFileSync(path.join(staging, "SOURCES.txt"), sourcesText(platform, spec));
  fs.writeFileSync(path.join(staging, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  fs.writeFileSync(path.join(staging, ".stamp.json"), `${JSON.stringify(stamp)}\n`);

  fs.rmSync(outDir, { recursive: true, force: true });
  fs.renameSync(staging, outDir);
  fs.rmSync(work, { recursive: true, force: true });
  console.log(`vendor: ${platform} ready in ${path.relative(root, outDir)}`);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    console.error(`vendor: ${error.message}`);
    process.exit(1);
  });
}
