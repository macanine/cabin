# Cabin

Cabin runs coding agents inside one Ubuntu virtual machine. Manage workspaces, chats,
terminals, and a Linux desktop from a Tauri 2 app. The guest service, `cabind`, supervises
sessions so they can keep running when the desktop app closes. The desktop shell runs on
Apple Silicon macOS and on Windows and Linux development hosts. The guest runs ARM64 on
Apple Silicon and follows the host architecture on Windows and Linux.

## Quick start

Use macOS 14 or later, stable Rust, Node.js 22.18 or later, and Zig for the Linux guest build.

```bash
brew install zig
cargo install cargo-zigbuild
rustup target add aarch64-unknown-linux-musl
npm ci
npm run app:dev
```

In the installer, choose Ubuntu 24.04 or 26.04 LTS and size the VM, set the guest account,
then review the configuration. The app uses the bundled QEMU when available. A macOS
development build without vendored QEMU can download the pinned engine during setup. The
installer downloads and verifies a dated Ubuntu cloud image, provisions the guest, and
shows progress and logs with cancel and retry controls.
[Architecture and release inputs](PLAN.md#architecture)

## Workspaces and sessions

- **Projects.** Start with an existing Mac folder or create a workspace on the VM. For a Mac
  folder, choose **Safe** to preserve guest edits, **Two-way** to share edits in both
  directions, or **Mirror** to make the guest copy match the Mac. Review conflicts per file.
  Sync respects `.gitignore`, `.cabinignore`, and excluded paths. Safe and Two-way
  copy `.git` once by default. **Settings → Mac permissions** shows folder access and lets
  you request it. Two-way uses read and write access on the host. On Windows it stops
  before syncing when names collide under Windows path rules, including directory names
  that differ only by case. Rename the conflicting file or folder to a distinct name,
  or exclude it from sync. Windows Two-way still checks the full included tree before
  applying changes; Cabin's temporary files do not trigger sync.
- **Tasks.** Choose a workspace and agent in **New Task**, then enter a prompt. The sidebar
  groups tasks by project, session, recency, or unread state and lists tasks that need you
  first. Press ⌘K (Ctrl+Shift+K elsewhere) to find a task or open a page. New Task remembers
  the last model you picked for each agent. For a Git workspace,
  **Worktree** starts from the selected committed ref in a detached checkout inside the VM.
  The session shows its path and offers cleanup while stopped. Cleanup requires a clean
  worktree and commits retained by a branch or tag. Archiving a session keeps its checkout.
- **Chat and terminals.** Chat shows turns, approvals, questions, plans, delegated work, and
  file changes. Send a message to resume a saved conversation. Terminal opens a login shell
  or the agent's native CLI and restores recent output on reconnect. Use the terminal footer
  or Command/Ctrl with `+`, `-`, or `0` to change or reset its saved text size. Paste an image
  with Command-V to save it as a guest attachment and paste its path. The session workbench opens
  files, shows Git changes, and previews a guest loopback service.
- **Commands, skills, and MCP.** In Chat, use `/` to find skills, agent commands, and
  Cabin's session commands. Use `$` as a shortcut for dollar-style skills and `@` for files.
  Before a session starts, the picker offers skills and files. Enter or Tab chooses a suggestion,
  and Esc closes the picker. Run `/cabin:status` or `/cabin:context` with Enter to open a
  read-only panel without sending a message to the agent. When a session offers `/mcp`,
  the agent adapter reports its status and supported actions in the conversation. The **MCP**
  page manages each agent's user-level config with backups. **Skills**
  manages shared installs from GitHub, skills.sh, ZIPs, and existing agent folders. Find
  project skills under **Agents → Setup**.

## Host CLI (macOS / Linux)

Start Cabin desktop and its VM, then run `cabin claude`, `cabin codex`, `cabin agy`,
`cabin opencode`, `cabin pi`, `cabin shell` (or `cabin sh`), or `cabin exec <cmd>` from a
local project. Existing Two-way mappings, including subdirectories, are reused. On the first
interactive run, Cabin shows both paths and asks before creating a Two-way binding under
`~/projects/` in Ubuntu. It uses the containing Git root, or the current directory if there is
none. Use `cabin bind` to opt in explicitly before running a script, or supply an absolute guest
destination with `cabin bind /home/<guest-user>/projects/<name>`. `cabin unbind` removes the
mapping without deleting either copy of the files. Run `cabin status` or `cabin list` to inspect
channel readiness and bound folders.

Manage the `cabin` launcher, install directory, shell `PATH` setup, passthrough options, VM
channel recovery, and folder bindings in **Settings → Host CLI**. On Unix hosts, first-run
desktop startup refreshes the managed `~/.local/bin/cabin` launcher without replacing another
command of that name or modifying shell rc files. The CLI and desktop must use the same
`CABIN_HOME` if you override the default. A VM upgrading from before the native channel was
added needs one cold restart.

The desktop manages the VM and the project sync lease. During the command, terminal and file
payloads go directly through private QEMU virtio-serial lanes to `cabind` with event-driven
`SIGWINCH` resizing, safe terminal environment forwarding, and adaptive guest idle backoff;
neither uses SSH. The desktop still uses SSH for machine setup and its existing RPC. Agent
credentials, sessions and CLI paths remain in the VM.
[Transport design and benchmark](docs/native-cli-prototype.md)

## Agents and usage

**Agents** lists each CLI with its sign-in state and available plan limits. Select one to
install or update it, open **Account** for its provider, accounts and limits, or open **Setup**
to manage its chat runtime and extensions. **Sign in via CLI** opens a dedicated terminal and
shows the next step for the provider. Cabin creates and reuses `~/workspace` under the guest
user's home for terminals opened from Agents. Return to **Account** to check the sign-in.
All chats use the [Agent Client Protocol](https://agentclientprotocol.com).

| Agent | Chat runtime | Sign-in and limits |
|---|---|---|
| Claude Code | `@agentclientprotocol/claude-agent-acp` runs the Setup CLI | Sign in through Claude Code. Account shows the current email, organization, plan, and `/usage` windows. |
| Codex | `@agentclientprotocol/codex-acp` runs the Setup CLI | The CLI shows a device code for browser authorization. Account supports `auth.json`, CLIProxyAPI, and Sub2API import and export. Switch accounts while Codex is idle. Exports use private files on the Mac. |
| OpenCode v2 | `opencode2 acp` from `@opencode/cli` | Uses OpenCode's native account and configuration. |
| Pi | `pi-acp` runs the Setup CLI | Enter `/login` in Pi to choose a provider. |
| Google Antigravity | `agy acp` from the managed Linux ARM64 release | `agy acp --login` handles sign-in. The live session supplies model and thinking choices. |

**Agents → Setup → Chat runtime** manages versions of the npm-based runtimes. The first chat
installs the selected runtime if needed. See
[runtime packages and pins](core/src/agent_runtime/runtimes.rs) and the
[Antigravity release pin](core/src/agents/antigravity.rs).

Antigravity installs `agy` at `~/.local/bin/agy`. You can also run `agy acp --login` in a
guest terminal. User MCP and skills live in `~/.gemini/config/mcp_config.json` and
`~/.gemini/config/skills/`. The CLI discovers project MCP in `.agents/mcp_config.json` and
project skills in `.agents/skills/`.
[Google ACP guide](https://antigravity.google/docs/ide/extensions/acp/) ·
[Google MCP reference](https://antigravity.google/docs/mcp/) ·
[Google customization discovery](https://antigravity.google/docs/customization-discovery/)

**Usage** shows cost, requests, tokens, cache hit rate, and daily trends by agent, model,
and period. It imports native agent records, including Antigravity's completed generations.
Costs use agent-reported values when available and API-equivalent estimates from models.dev
or your custom prices otherwise. Subscription limits appear in **Agents** or the provider
CLI. The home page and sidebar share today's usage totals.
[Antigravity quota guide](https://antigravity.google/docs/cli/commands/usage)

## Network and desktop

On macOS and Windows the VM gateway follows the host system route, including an active VPN
or TUN proxy. Add an optional proxy chain in **Settings → Network**. Apply route changes
while the VM is connected and internet access is resumed. The route becomes active after
Network Check passes. Open **Nodes** to test a node’s latency or test all visible nodes.
Manage subscriptions from the link icon.
[Network architecture](PLAN.md#network-routing-and-recovery)

The sidebar shows network and machine metrics while the VM is connected. **Network Check**
explains the next action and keeps raw observations and history in expandable sections.
Choose **Static IP** (the default) to require a single public IPv4 address. Choose **Dynamic
residential IP** if the route rotates addresses within a verified country and continent. A
confirmed violation closes the gateway. A normal check retains the approved exit, and
**Approve new exit** replaces it only after a successful full check. Baselines persist per
exit and policy across restarts. Monitoring samples the route periodically.
Guest DNS goes through the selected route over HTTPS. Use **Region** to set the VM's time
zone and language.

Install **Chrome** to open a Sway desktop inside the app. **Apps** launches each installed
GUI app in its own Sway workspace. **Environment** sets Chrome's region and identity. Region
changes apply live and identity changes restart a running Chrome. Select **Open Chrome** to
start the browser. Quitting Chrome leaves it stopped. VNC over SSH is the default display.
Machines without the device profile can enable SPICE, which adds a virtual GPU on the next
restart. Both viewers use the same desktop.

## Machine resources, recovery and storage

In **Settings → Machine → Change allocation**, set the vCPU count, memory in MiB, or a
larger total disk capacity in GiB. Save while the machine is running or stopped. If it is
running, stop it and then start it to use the new allocation. Saving does not stop it.

The disk can only grow. Cabin expands its standalone qcow2 image before boot, then grows and
verifies the Ubuntu root partition and ext4 filesystem. You can cancel a queued expansion
only before the image has changed, while the machine is stopped. If guest expansion is not
verified, Machine settings shows the error and the next cold start retries it without
shrinking the disk. Custom root layouts, such as LVM or encrypted volumes, are not supported.

Open **Machine recovery** from a startup error or **Settings → Machine** to review the start
log and verify or repair the installation. Repair preserves the VM disk. **Erase and
reinstall** removes app-owned machine data after confirmation and keeps the original Mac
project folders.

**Settings → Storage** shows the VM disk, images, QEMU, application bundle, logs, settings,
and WebKit data. With the VM stopped, **Clean up** can remove leftover downloads, redundant
QEMU archives, and eligible diagnostic captures older than 30 days.

### Diagnostics

Cabin stores host logs in `<host data>/vm/logs` and guest logs in `<guest data>/logs`.
`events.jsonl` holds structured events with UTC Unix milliseconds, severity, process,
component, and event name. On the host, `operations.jsonl` records VM operations used by
installation recovery. Both streams rotate at 2 MiB and keep three previous segments.
Set `CABIN_LOG_LEVEL=debug` on a process to include debug events such as sync timings.
The default level is `info`. The `warn` and `error` levels reduce logging. Read the
recent host stream with `vmctl --home <host-data>/vm logs --events`.

QEMU serial output and installer output are available through the startup report and
`vmctl logs`. Raw captures use files scoped to the operation or process. The serial and
QEMU stderr captures start fresh when QEMU launches, and Storage can clean old captures
with the VM stopped. Machine diagnostics include bounded excerpts and recent events
from the host and, when connected, the guest. The export redacts known local paths,
account names, and URL credentials. Review raw excerpts before sharing a report.

## Development

| Task | Command |
|---|---|
| Run the desktop app (macOS or Windows) | `npm run app:dev` |
| Build the app (`.app` on macOS, NSIS installer on Windows) | `npm run app:build` (add `-- --debug` for a debug build) |
| Run frontend tests | `npm test` |
| Lint the frontend | `npm run lint` |
| Lint, type-check and test the frontend | `npm run check` |
| Run the UI smoke test (needs Chromium; `CHROME` picks one) | `npm run test:ui` |
| Type-check and bundle the frontend | `npm run build` |
| Run Rust tests | `cargo test --manifest-path <crate>/Cargo.toml` for `core`, `diagnostics`, `usage`, `vm`, `src-tauri`, and `ip-detection` |
| Check Rust formatting and lints | `cargo fmt --manifest-path <crate>/Cargo.toml --check` and `cargo clippy --manifest-path <crate>/Cargo.toml --all-targets -- -D warnings` |
| Build the Linux ARM64 guest daemon | `npm run build:agentd` |

CI runs formatting, clippy, ESLint, the type check and the test suites on every push. The
live VM suites under `vm/tests/*_live.rs` need QEMU/HVF and a prepared image, so
[the live workflow](.github/workflows/live.yml) runs them nightly on a self-hosted macOS
runner, one suite per job, once the repository variable `CABIN_LIVE_CI` is true.

On a fresh macOS or Windows checkout, run `npm run vendor` and `npm run build:agentd` before
running the `src-tauri` tests directly. Tauri validates both resources while compiling
the test binary.

`app:dev` and `app:build` run `scripts/tauri.mjs`, the one launcher for every host. It puts
Cargo on `PATH`, checks the guest daemon toolchain (Rust musl target, Zig, `cargo-zigbuild`)
before compiling, and runs the local Tauri CLI. Tauri merges `src-tauri/tauri.macos.conf.json`
or `tauri.windows.conf.json` by itself. Rust build artifacts go to
`~/Library/Caches/cabin/target-macos` on macOS and `%LOCALAPPDATA%\cabin\target-windows` on
Windows unless `CARGO_TARGET_DIR` is set, so a tree shared with another OS keeps each
platform's binaries apart. `npm run tauri:mac -- <args>` and `npm run tauri:win -- <args>` pass
any Tauri command through the same launcher and stop on the wrong host. If you share
`node_modules` with a Linux checkout, `npm run setup:mac` adds the Mac's native packages,
while `npm ci` replaces the dependency directory.

Linux development uses the distribution's QEMU with KVM or TCG. Use `npm run tauri dev` for
the Linux development shell or `npm run tauri build` for Linux packages. Tauri builds the
guest daemon before running or packaging the app. The macOS bundle includes the daemon,
VM configuration, assets, and vendored QEMU.

### Windows host

On Windows x64, Cabin runs the amd64 Ubuntu guest. TCG software emulation uses longer
startup and provisioning timeouts. Requirements:

- Rust 1.100 or newer for the Windows host build.
- Microsoft C++ Build Tools and WebView2.
- Zig (`winget install zig.zig`), `cargo install cargo-zigbuild`, and
  `rustup target add x86_64-unknown-linux-musl` to build the guest daemon.

Then run `npm ci` and `npm run app:dev`, the same commands as on macOS. QEMU and OpenSSH come
from `npm run vendor` (pinned and hash-checked, see [Packaging](#packaging)). The app never
uses a QEMU on `PATH`, and prefers the bundled `ssh` over the system one.

Windows shares the AF_UNIX transport used by the other hosts for QMP, the guest agent,
the serial console, and the QEMU gateway stream. The loopback SSH relay starts at port 12233
by default and tries successive ports until it can listen. Inside the gateway, connected loopback
UDP sockets carry IP packets to the shoes proxy stack. If setup reports that a VM socket
path is too long, set `CABIN_HOME` to a shorter local data directory before restarting Cabin.
SPICE requires a supported Unix host. `tauri.windows.conf.json` builds
the NSIS installer with the daemon and VM resources bundled. See
[host platform contracts](AGENTS.md#host-platforms).

The downloader uses Windows system proxy settings, including PAC and automatic proxy
discovery. When Windows resumes from sleep, Cabin checks the guest SSH link and re-verifies
the network route before restoring access.

### Opt-in verification

Run the Mac interface test with an active IPv4 adapter. It compares real DNS and HTTPS
observations over the gateway and system route, then rebuilds the bound path with the SSH
relay still listening.

```bash
CABIN_TEST_INTERFACE=en0 cargo test --manifest-path vm/Cargo.toml \
  --test gateway_interface_live -- --ignored --nocapture
```

Before uploading the Windows installer, CI installs it into a temporary directory, checks
the resource hashes, and exercises QMP, gateway AF_UNIX, and shutdown with the installed
engine. The build also boots a pinned Ubuntu image under TCG, authenticates over SSH,
checks the installed `cabind` handshake, syncs in both directions, and repeats after a VM
restart. The TCG fixture restores a saved fallback decision. The same test runs under
WHPX in the [live workflow](.github/workflows/live.yml) when `CABIN_WINDOWS_LIVE_CI` is
`true`, a `Windows`, `X64`, `cabin-live` runner is available, and
`CABIN_WINDOWS_TEST_IMAGE` points to a local Ubuntu amd64 qcow2 image on that runner.

To run the shared live sync test, set `CABIN_VENDOR_DIR`, `CABIN_TEST_IMAGE`,
`CABIN_TEST_AGENTD`, and `CABIN_TEST_ACCEL` (`hvf` on macOS, `whpx` or `tcg` on Windows).
The fixture writes to a disposable overlay over the supplied image.

```text
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib sync::tests::vm_live::ubuntu_ssh_sync_and_restart -- --exact --ignored --nocapture
```

The [VM integration tests](vm/tests/) document their prerequisites. The account test uses a
disposable Ubuntu VM, the installed QEMU engine, a freshly built daemon, and synthetic
accounts and stub CLIs. The older `gateway_vm_live` and `kernel_vm_live` harnesses use
Homebrew QEMU and its firmware for their disposable VMs. The desktop app downloads its own
engine.

OpenCode v2 covers the live ACP flow. Point the test at an installed `opencode2` binary.
Claude Code, Codex, Pi, and Antigravity use offline checks.

```bash
CABIN_LIVE_OPENCODE=1 CABIN_OPENCODE2_BIN=/path/to/opencode2 \
  cargo test --manifest-path core/Cargo.toml --test live_opencode -- --ignored --nocapture
```

### Packaging

Release builds bundle QEMU and the guest daemon. On Windows, the NSIS installer downloads
WebView2 if the runtime is missing, which requires internet access. `scripts/vendor.json`
pins QEMU for macOS arm64 and Windows x64, and Win32-OpenSSH for Windows x64 (url, sha256,
version, license, source URL). `npm run vendor` (also run by Tauri's `beforeDevCommand` and
`beforeBuildCommand`) downloads the pins into `build/vendor-cache/`, verifies the hashes, and
unpacks a pruned tree into `build/vendor/<platform>/` with `manifest.json`, `SOURCES.txt`, and
license files. Tauri bundles that directory as the resource `vendor/`. The manifest format is
documented at the top of `scripts/fetch-vendor.mjs`.

To bump a pin, edit the entry in `scripts/vendor.json`: download the new file, compute its
SHA-256 yourself (`shasum -a 256`), and update `url`, `sha256`, `version`, and `sourceUrl`
together. For the macOS QEMU, keep `vm/src/qemu_engine.rs` (`RELEASE`, `SHA256`) in step until
it reads the manifest. Then run `npm run vendor` and check the tree. `npm run app:build` on
Windows builds the installer, and CI (`.github/workflows/build.yml`) builds both platforms.
Linux uses the distribution's QEMU and skips the fetch.

### IPC bindings

Regenerate the TypeScript bindings after changing Rust wire types or Tauri commands.

```bash
CABIN_UPDATE_IPC_BINDINGS=1 cargo test --manifest-path core/Cargo.toml ipc_bindings
CABIN_UPDATE_IPC_BINDINGS=1 cargo test --manifest-path src-tauri/Cargo.toml ipc_bindings
```

## State and project documentation

The desktop app uses `CABIN_HOME` when set. Its defaults are
`~/Library/Application Support/cabin` on macOS, `%APPDATA%\cabin` on Windows, and
`$XDG_DATA_HOME/cabin-host` or `~/.local/share/cabin-host` on Linux. This directory holds VM
files and host settings.

The guest resolves its own data directory from `CABIN_HOME`, then `XDG_DATA_HOME/cabin`,
then `~/.local/share/cabin`. It holds workspace and session data, tool state, configuration
backups, and `usage.sqlite3`. Agent configuration stays under the guest user's home. Guest
Node.js builds default to `/opt/node`, with `CABIN_NODE_ROOT` available as an override.

- [AGENTS.md](AGENTS.md) maps features to source files and records implementation contracts.
- [PLAN.md](PLAN.md) describes the architecture, design tradeoffs, and open work.
- [IP detection library](ip-detection/README.md) documents the standalone lookup API and
  providers.
