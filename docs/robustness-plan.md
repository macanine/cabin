# Robustness plan: install and run on every supported machine

Goal: a fresh Cabin install works on any supported Mac (Apple Silicon, macOS 14+) and
Windows PC (Windows 10 22H2+/11, x64). Every failure along the way is
retried when it is transient, explained when it is not, and never leaves the machine in a state
that a retry or restart cannot fix.

Each chain below lists its failure modes. Every item needs a fix, or a test that proves it is
already handled, or a documented reason why it cannot be handled. Use temp dirs,
loopback servers and `FakeSystem` for tests (see AGENTS.md "Verification").

## R1: Install inputs (download, image, disk, seed)

Files: `vm/src/download.rs`, `vm/src/os.rs`, `vm/src/mirror.rs`, `vm/src/steps/{global,disk,seed}.rs`, `vm/src/iso.rs`.

- Download: DNS failure, TLS failure (corporate MITM, clock skew), proxy variables
  (`HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, Windows system proxy), captive portal (an HTML page
  served instead of the image), a server without Range support, `Content-Length` mismatch,
  stalls with no bytes for N seconds, a mid-download mirror switch, disk full mid-download,
  a `.part` file larger than the target, and a concurrent second install. Retries use backoff,
  each mirror is tried, and the error names the real cause.
- Checksum: `SHA256SUMS` unreachable, the official host and the mirror disagree, a malformed
  line, or a mirror one build behind the official host.
- Disk: `qemu-img create` fails (disk full, path not writable, AV lock on Windows), the
  requested size exceeds free space (refuse before starting), or the path contains spaces or
  non-ASCII characters (user profile names).
- Seed ISO: usernames, hostnames and SSH keys with unusual characters are validated
  before writing. The ISO round-trip test runs on both platforms.

## R2: VM lifecycle and recovery

Files: `vm/src/{vm,machine,lock,qmp,sockets,startup,agentd,journal}.rs`, `src-tauri/src/{host,machine_service}.rs`.

- Start: stale AF_UNIX socket files from a crash are unlinked before bind. Socket paths
  stay under the platform length limit for long or non-ASCII home paths. The SSH port is taken
  by another process. QEMU exits immediately (capture stderr and classify it). WHPX
  fails, so fall back to TCG once and remember why.
- Stale state after a crash or power loss: a lock file held by a dead pid, a pid
  file pointing at a reused pid, a half-written JSON (use atomic writes everywhere), or a
  `.verified.json` that no longer matches.
- Connection: SSH never comes up (the guest is slow under TCG; timeouts scale with
  `slowdown()`), a host key change after a reinstall, a cabind deploy that fails mid-copy (atomic
  rename on the guest), a hello build mismatch loop (deploy once, then report), a
  QMP or guest agent hang (every call has a timeout).
- Stop and quit: the app quits during an install, start or stop; the VM is shut down
  gracefully and then forcibly; host sleep and wake; Windows logoff.
- Migration: instances created by earlier builds (`sockets.json` TCP ports from the
  Windows port, the downloaded mac QEMU engine, legacy Debian configs) start or report
  a clear upgrade path.

## R3: Host readiness, packaging and first launch

Files: `vm/src/{readiness,accel,platform,vendor,qemu_engine}.rs`, `scripts/fetch-vendor.mjs`,
`src-tauri/tauri*.conf.json`, `src/views/install/*`, `src/lib/wizard.ts`.

- An unsupported host fails at first launch with a precise message: Intel Mac, macOS < 14,
  Windows on ARM, 32-bit, or Windows < 10 22H2.
- macOS: the vendored QEMU needs the `com.apple.security.hypervisor` entitlement once
  signed. Quarantine xattrs on bundled binaries, and a translocated app (run from the DMG).
- Windows: WHPX off, VT-x off in firmware, Hyper-V/VBS conflicts, antivirus quarantining
  or blocking `qemu-system-x86_64.exe`, SmartScreen, OneDrive-redirected or roaming
  `%APPDATA%`, long paths, and a non-ASCII user profile.
- Low memory or disk: the size preset never exceeds what the host can give, and readiness
  rechecks after a fix.
- Every backend error that can reach the installer maps to a plain explanation and an action
  in `explainFailure`. Unit-test each mapping with the real error strings from R1 and R2.

## R4: Diagnostics

- One "Copy diagnostics" action collects versions, host facts, readiness, the last
  install and start reports, and log tails, with paths and user names redacted.

## Done means

- macOS: `npm test`, `npm run test:ui`, the journey scenarios, and `cargo test` in core, vm and src-tauri.
- Windows: `npm run check:win` is warning-free, and CI is green on both platforms.
- Each workstream reports, per item, whether it was fixed, already handled (with the test that proves it), or not
  possible, with the reason.
