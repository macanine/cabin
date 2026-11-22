# Install and multi-platform overhaul

Base: `windows-x64` (d4015b6). Working branch: `install-overhaul`.

## Goal

A user downloads one installer for their platform, opens Cabin, presses **Install**, and
gets a working VM. They install nothing by hand: no QEMU, no OpenSSH, no xorriso,
no Homebrew. The guest runs the host's native architecture with hardware acceleration
(macOS Apple Silicon → arm64 guest on HVF; Windows x64 → amd64 guest on WHPX). Software
emulation is a visible, explained fallback, never a silent default.

## Target install flow

1. **Download.** `Cabin_<ver>_aarch64.dmg` (macOS) or `Cabin_<ver>_x64-setup.exe` (NSIS,
   Windows). Each bundle already contains:
   - the pinned QEMU for that platform (binaries, DLLs/dylibs, EDK2 firmware, licenses),
   - `cabind` built for the guest architecture (`aarch64-` / `x86_64-unknown-linux-musl`),
   - on Windows, a pinned Win32-OpenSSH (`ssh.exe`, `ssh-keygen.exe`).
2. **Readiness (automatic, first screen).** Cabin checks the host and lists only real
   problems, each with a one-click fix or an exact instruction:
   - hardware acceleration: HVF (macOS); WHPX available *and* actually usable (probe
     launch), otherwise "Turn on Windows Hypervisor Platform" (elevated
     `dism /online /enable-feature /featurename:HypervisorPlatform /all /norestart`,
     then "restart Windows") or "enable virtualization in firmware",
   - free disk space for the chosen disk size plus image, RAM for the chosen preset,
   - bundled tools present and runnable (QEMU, ssh, cabind). A missing bundled tool is a
     broken install, reported as "Reinstall Cabin", not "install QEMU yourself".
3. **One-screen setup.** Defaults are chosen for the user: Ubuntu 24.04 LTS, a size preset
   picked from host RAM/CPU, username from the host account, package and image mirror
   picked by latency. Everything else lives under **Advanced** (release, exact sizes,
   hostname, mirror, device profile on arm64 only). Primary button: **Install**.
4. **Progress.** Image download (resumable, checksum-verified, fastest mirror with
   fallback), disk, seed, boot, provisioning, desktop, browser. Real ETA, plain-language
   step names, logs folded until failure, retry/cancel.
5. **Done.** Lands on the start page with the machine running.

## Defects found in `windows-x64`

1. `vm/src/vm.rs::preflight` reports "this build targets arm64" on any non-arm64 host with
   acceleration. With WHPX working the install is refused.
2. Windows requires a user-installed QEMU from PATH/Program Files (`platform::distro_qemu`).
   `qemu_engine::Engine::in_tree` hardcodes `qemu-system-aarch64` and aarch64 firmware.
3. WHPX detection checks only that `WindowsHypervisorPlatform.dll` exists. It does not
   prove WHPX works (Hyper-V conflicts, firmware VT-x off). It passes no `kernel-irqchip=off`,
   which current QEMU+WHPX on q35 commonly needs. There is no fallback when the WHPX launch fails.
4. Requires the Windows OpenSSH client to be installed and on PATH.
5. `scripts/build-agentd.mjs` and `src-tauri/src/lib.rs::agentd_binary` pick the guest
   triple from the *build host*, not the bundle target; cross-building breaks.
6. Architecture is decided ad hoc in ~15 places (`if guest_arch() == "x86_64"`). Still
   hardcoded to arm64: `vm/config/browser.json` UA/platform, `vm/src/browser.rs`
   (`"Linux aarch64"`), `src-tauri/src/network_gate.rs`, `vm/src/storage.rs`
   (`ubuntu-24.04` partial name), `vm/src/machine.rs` and `vm/src/storage.rs` engine
   file lists, `vm/src/cli.rs` (`qemu-system-aarch64 --version`), `vm/src/device/profile.rs`.
   `config::CHROME_URL` keys on the *host* `target_arch`.
7. Device profile is a hard error on x86 instead of being hidden.
8. The installer copy still assumes developers ("cabind for the VM: <path>", "Install
   qemu-system-x86…"). Readiness has no fix actions.
9. Cloud images come only from `cloud-images.ubuntu.com` (slow in some regions). Mirrors
   apply only to apt.
10. No CI. Nothing builds or tests the Windows target.

## Workstreams

Each workstream runs on its own branch off `install-overhaul` and owns the files listed.
It does not edit files owned by another workstream in the same wave.

### Wave 1

**A: Vendored binaries and packaging** (`ws-a-vendor`)
- `scripts/vendor.json`: per-platform pins (url, sha256, version, license, source URL)
  for QEMU macOS arm64 (the existing `macanine/qemu` asset), QEMU Windows x64
  (latest stable qemu.weilnetz.de `qemu-w64-setup-*.exe`, extracted at build time), and
  Win32-OpenSSH x64 (`PowerShell/Win32-OpenSSH` release zip).
- `scripts/fetch-vendor.mjs`: downloads, verifies sha256, and extracts into
  `build/vendor/<platform>/{qemu,openssh}`. It keeps only what Cabin needs (the
  `qemu-system-<arch>` binary, `qemu-img`, required DLLs, the edk2 firmware for the
  guest arch, licenses). It caches by sha and is idempotent. NSIS extraction uses the
  `7zip-bin` npm package. It writes `build/vendor/<platform>/manifest.json`
  (relative paths of qemu, qemu-img, firmware code/vars, ssh, ssh-keygen, versions).
- `scripts/build-agentd.mjs`: guest triple from `--target`/`TAURI_ENV_TARGET_TRIPLE`/
  `CABIN_GUEST_ARCH`, falling back to the host.
- `src-tauri/tauri.macos.conf.json`, `tauri.windows.conf.json`: bundle
  `build/vendor/<platform>/` as resource `vendor/`. `beforeBuildCommand` runs the fetch.
  `package.json` scripts: `vendor`, `tauri:win`.
- `.github/workflows/build.yml`: matrix macos-14 (arm64) and windows-latest (x64). It runs
  frontend tests, `cargo test` for core/vm/src-tauri, the guest cross-build, vendor fetch,
  and `tauri build`, then uploads the installers as artifacts.
- QEMU GPL compliance: ship `COPYING`/license files and a `SOURCES.txt` with the source URL.

**B: Guest architecture model and acceleration** (`ws-b-arch`)
- New `vm/src/arch.rs`: `enum GuestArch { Arm64, Amd64 }` with `current()`,
  `debian()` (`arm64`/`amd64`), `rust_triple()`, `qemu_system()`, `firmware_names()`,
  `machine_type()`, `chrome_deb_url()`, `ua_platform()` (`Linux aarch64`/`Linux x86_64`),
  `apt_mirrors()`. Replace every ad-hoc arch branch in `vm/` and `src-tauri/src/lib.rs`
  with it. Fix defect 6 in full.
- Fix the `preflight` arch check (defect 1).
- Acceleration: `-accel whpx,kernel-irqchip=off` on Windows. Add a probe
  (`qemu -accel whpx -machine q35 -nodefaults -S -display none` for a few seconds,
  via a function that takes the engine path) and cache its result with the reason. Expose
  `AccelStatus { accel, emulated, reason: Option<AccelProblem> }`, where `AccelProblem` is one of
  `WhpxFeatureOff | VirtualizationOff | HypervisorConflict | ProbeFailed(String)`. On a
  WHPX launch failure at VM start, retry once under TCG and record why.
- Browser UA and platform per arch: `vm/config/browser.json` becomes arch-neutral or
  gets per-arch values, plus `vm/src/browser.rs` and the `network_gate.rs` fixture.
- Device profile: refuse it only as an option validation. The UI hides it (wave 2).
- Unit tests for every mapping. Mac `cargo test` stays green.

### Wave 2 (after A and B are merged)

**D: Runtime uses the bundled tools** (`ws-d-runtime`)
- `qemu_engine.rs`: resolve the engine in order: (1) bundled `<resources>/vendor/qemu`
  via the manifest, (2) the legacy downloaded mac engine, (3) distro QEMU (Linux dev only).
  Windows never searches PATH or Program Files. The installer `qemu` step becomes
  "verify bundled engine".
- SSH: `util::which("ssh")` callers resolve bundled `vendor/openssh/ssh.exe` first on
  Windows. Update `platform::required_tools`.
- `ImageStep`: cloud image mirrors (`cloud-images.ubuntu.com`, then
  `mirrors.tuna.tsinghua.edu.cn/ubuntu-cloud-images`, `mirrors.ustc.edu.cn/ubuntu-cloud-images`)
  chosen by latency, with fallback on failure. Always verify against `SHA256SUMS` fetched
  from the official host when it is reachable, otherwise from the mirror.

**E: Readiness and installer UX** (`ws-e-ux`)
- New `vm/src/readiness.rs`: `host_readiness(home, plan) -> Vec<Readiness>` with
  `{ id, label, ok, detail, fix: Option<Fix> }`, where `Fix` is one of `EnableWhpx |
  RestartHost | ReinstallApp | FreeDisk | OpenUrl(String)`. Map it in
  `src-tauri/src/host.rs::checks`, plus a `host_fix` command. The Windows elevated `dism`
  runs through `ShellExecuteW` with `runas`.
- Frontend (`src/views/InstallWizard.tsx`, `src/views/install/*`, `src/lib/installForm.ts`,
  `src/lib/wizard.ts`): a single setup screen with auto defaults and an Advanced fold, as
  in step 3 above. Platform-neutral copy everywhere ("this computer"). Device profile is
  hidden on amd64. The readiness list shows fix buttons. Regenerate ts-rs bindings.
- Tests: `tests/installForm.test.mjs` and `tests/wizard.test.mjs` cover preset auto-pick and
  readiness rendering.

### Later (not scheduled)

- Prebuilt, pre-provisioned Cabin base image per arch (desktop and Chrome preinstalled),
  published as a release asset, to cut install time from minutes to a download.
- Signing: notarize the mac bundle including vendored dylibs, and code-sign the NSIS installer.
- Replace the system ssh with an in-process SSH client to drop the OpenSSH bundle.

## Verification

- macOS: `npm test`, `cargo test` in `core`, `vm`, and `src-tauri`, `npm run tauri:mac -- build`.
- Windows: the CI job (build, unit tests, NSIS artifact). A live install on a Windows 11 x64
  machine with WHPX on and then off covers the end-to-end flow.
