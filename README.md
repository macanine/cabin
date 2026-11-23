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
  you request it. Two-way uses read and write access on the Mac.
- **Tasks.** Choose a workspace and agent in **New Task**, then enter a prompt. The sidebar
  groups tasks by project, session, recency, or unread state. For a Git workspace,
  **Worktree** starts from the selected committed ref in a detached checkout inside the VM.
  The session shows its path and offers cleanup while stopped. Cleanup requires a clean
  worktree and commits retained by a branch or tag. Archiving a session keeps its checkout.
- **Chat and terminals.** Chat shows turns, approvals, questions, plans, delegated work, and
  file changes. Send a message to resume a saved conversation. Terminal opens a login shell
  or the agent's native CLI and restores recent output on reconnect. Use the terminal footer
  or Command/Ctrl with `+`, `-`, or `0` to change or reset its saved text size. Paste an image
  with Command-V to save it as a guest attachment and paste its path. The session workbench opens
  files, shows Git changes, and previews a guest loopback service.
- **Commands, skills, and MCP.** Use `/` for the agent's commands, `$` for skills, and `@`
  for workspace files. The **MCP** page imports or creates servers and updates each agent's
  native config with backups. **Skills** manages shared installs from GitHub, skills.sh,
  ZIPs, and existing agent folders. Find project skills under **Agents → Setup**.

## Agents and usage

The **Agents** page shows each CLI's version, sign-in status, and available plan limits.
Open an agent's **Account** or **Setup** tab to manage it. All chats use the
[Agent Client Protocol](https://agentclientprotocol.com).

| Agent | Chat runtime | Sign-in and limits |
|---|---|---|
| Claude Code | `@agentclientprotocol/claude-agent-acp` runs the Setup CLI | Sign in through Claude Code. Account shows the current email, organization, plan, and `/usage` windows. |
| Codex | `@agentclientprotocol/codex-acp` runs the Setup CLI | Account supports `auth.json`, CLIProxyAPI, and Sub2API import and export. Switch accounts while Codex is idle. Exports use private files on the Mac. |
| OpenCode v2 | `opencode2 acp` from `@opencode/cli` | Uses OpenCode's native account and configuration. |
| Pi | `pi-acp` runs the Setup CLI | Uses Pi's native account and configuration. |
| Google Antigravity | `agy acp` from the managed Linux ARM64 release | Select **Open terminal** in Account to sign in. The live session supplies model and thinking choices. |

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

Use **Settings → Network** to choose the Mac's system route or bind the VM gateway to a
physical interface. The optional proxy chain uses that interface for its first hop. You can
apply an interface beside its picker, save it while the VM is stopped or disconnected, or
save it while internet access is paused. A live change becomes active after Network Check
passes. If the selected interface disconnects, the gateway keeps SSH available and closes
internet access while it waits for a fresh check. **System route** follows macOS routing,
including its active VPN. [Network architecture](PLAN.md#network-routing-and-recovery)

The sidebar shows the gate state, observed exit, and traffic. Open **Network Check** to see
the current blockers, their fixes, and verification history. Guest DNS goes through the
selected route over HTTPS. With a selected adapter, bootstrap DNS for proxy and resolver
names uses UDP on that adapter and needs access to public DNS. Use **Region** to set the
VM's time zone and language.

Install **Chrome** to open a Sway desktop inside the app. **Apps** launches each installed
GUI app in its own Sway workspace. **Environment** sets Chrome's region and identity. Region
changes apply live and identity changes restart Chrome. VNC over SSH is the default display.
Machines without the device profile can enable SPICE, which adds a virtual GPU on the next
restart. Both viewers use the same desktop.

## Machine recovery and storage

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
| Type-check and bundle the frontend | `npm run build` |
| Run Chromium smoke tests with mocked IPC | `npm run test:ui` |
| Run Rust tests | `cargo test --manifest-path <crate>/Cargo.toml` for `core`, `diagnostics`, `usage`, `vm`, `src-tauri`, and `ip-detection` |
| Build the Linux ARM64 guest daemon | `npm run build:agentd` |

Set `CHROME` to a Chromium or Chrome executable for `npm run test:ui`. On a fresh
macOS or Windows checkout, run `npm run vendor` and `npm run build:agentd` before
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

- Microsoft C++ Build Tools and WebView2.
- Zig (`winget install zig.zig`), `cargo install cargo-zigbuild`, and
  `rustup target add x86_64-unknown-linux-musl` to build the guest daemon.

Then run `npm ci` and `npm run app:dev`, the same commands as on macOS. QEMU and OpenSSH come
from `npm run vendor` (pinned and hash-checked, see [Packaging](#packaging)). The app never
uses a QEMU on `PATH`, and prefers the bundled `ssh` over the system one.

Windows shares the AF_UNIX transport used by the other hosts for QMP, the guest agent,
the serial console, and the QEMU gateway stream. Inside the gateway, connected loopback
UDP sockets carry IP packets to the shoes proxy stack. If setup reports that a VM socket
path is too long, set `CABIN_HOME` to a shorter local data directory before restarting Cabin.
Windows uses system routing. Adapter binding and SPICE require a supported Unix host. `tauri.windows.conf.json` builds
the NSIS installer with the daemon and VM resources bundled. See
[host platform contracts](AGENTS.md#host-platforms).

### Opt-in verification

Run the Mac interface test with an active IPv4 adapter. It compares real DNS and HTTPS
observations over the gateway and system route, then rebuilds the bound path with the SSH
relay still listening.

```bash
CABIN_TEST_INTERFACE=en0 cargo test --manifest-path vm/Cargo.toml \
  --test gateway_interface_live -- --ignored --nocapture
```

Before uploading the Windows installer, CI installs it silently into a temporary directory,
checks the installed executable and resource hashes, and runs the native QEMU tests against
that installation's `vendor` directory. The tests cover QMP, gateway AF_UNIX, and shutdown
on whichever accelerator the runner selects; they do not guarantee coverage of both WHPX
and TCG. To run them locally on Windows, set `CABIN_VENDOR_DIR` to the installed `vendor`
directory or the staged `build/vendor/windows-x64` directory and run
`cargo test --manifest-path vm/Cargo.toml --test windows_qemu_live -- --ignored --nocapture`.

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
