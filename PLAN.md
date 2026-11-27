# Cabin architecture and plan

Cabin is a desktop workbench for one Ubuntu VM. The host provides the window,
machine lifecycle, network, and project sync. The guest owns the agent executables,
workspaces, PTYs, chat processes, and desktop session.

See [README.md](README.md) for setup, product behavior, and test commands. See
[AGENTS.md](AGENTS.md#implementation-index) for source entry points and implementation
contracts.

## Architecture

| Area | Decision |
|---|---|
| VM and release inputs | On Apple Silicon, Cabin runs an ARM64 guest under HVF with the bundled QEMU or the pinned macanine/qemu download for unbundled development. Windows x64 uses bundled QEMU with WHPX and can fall back to TCG. Linux uses its distribution's QEMU. Setup records the Ubuntu 24.04 or 26.04 LTS image's dated URL and SHA-256. Ubuntu's generic kernel supplies the required 9p and display drivers. |
| Host and guest transport | Tauri commands run on the host. Guest commands use newline-delimited JSON RPC and typed events over `ssh ... cabind connect`. The per-machine SSH key authenticates the relay. The app compares the daemon's hello build hash with its Linux binary for the guest architecture and deploys it when needed. QMP, the guest agent, the serial console, and QEMU's gateway stream use per-instance AF_UNIX sockets on every host. |
| Lifecycle and recovery | The installer collects System, Account, and Review before it starts provisioning. Guest system changes prepare, apply, verify, and roll back through the operation journal. Machine startup reports each phase and keeps its logs. A first-install rollback removes its incomplete disk, while repair uses the existing disk. |
| Desktop and tools | Sway is the guest desktop. noVNC over SSH is the default viewer. SPICE uses the same desktop through a private QEMU socket on supported machines. Node.js, Python, uv, and agent tooling are installed in the guest. |
| UI and data flow | The React window consumes a startup snapshot and typed events. Host and guest report authoritative process and operation state. Visible reads refresh on entry, foreground return, updates, or bounded polling where events are unavailable. Settings use one column with the most relevant state and actions at the top. |
| Project data | Agents work on the VM's local disk. Mac projects sync against a saved base. The VM supports 9p for optional live mounts outside project sync. |

## Native host CLI

The [native host CLI](docs/native-cli-prototype.md) forwards the five original
agent executables, interactive shells (`cabin shell` / `cabin exec`), and Two-way
project sync over three private QEMU virtio-serial lanes. **Settings → Host CLI**
manages the `cabin` launcher symlink, shell `PATH` detection, passthrough toggles,
VM channel restart, and folder bindings (`<data>/cli/config.json`). The host uses a
self-pipe with `SIGWINCH` and signal handlers for zero-polling terminal I/O and
resizing; guest channel workers apply adaptive idle backoff when disconnected and
graceful process-group shutdown for interactive sessions. The desktop starts the
guest workers during connection setup and leases its existing sync manager to the
CLI for one project at a time. The lease ends on explicit close or local socket
disconnect (including client cancel during initial sync). An interrupted CLI waits for
the guest to close the process before requesting final sync. Other desktop projects
and desktop RPC keep their existing SSH transport. Windows CLI and multi-session
native multiplexing are open work.

## Diagnostics

`cabin-diagnostics` supplies one event format and file policy to the desktop, `vmctl`,
and `cabind`. Host and guest each write to local storage so a broken SSH connection
still leaves diagnostics on both sides. The support report requests a bounded guest
event tail with a deadline and remains available without the guest. The renderer
submits allowlisted event identifiers and the host rate-limits them.

The operation journal uses the same rotating writer with durable appends. Recovery
reads its legacy location as well as the current journal. Keep raw captures for QEMU,
SSH, provisioning and managed desktop processes where a consumer needs the original
output. Open them with the shared capture API and use bounded reads in reports.
CLI stdout, protocol streams, and user terminal and agent output keep their own
contracts. App events should carry operational context and omit user content,
credentials, request payloads, and complete command arguments.

## Network routing and recovery

### Traffic paths

The VM's only network interface connects to the host gateway. The macOS and Windows apps use
the host system route for direct transport and the first hop of a proxy chain. The desktop
converts legacy adapter bindings to the system route on startup. Outbound verification and
diagnostic requests use the candidate or saved route they describe. Guest UDP DNS travels
through that route using DNS over HTTPS. The gateway forwards public IPv4 traffic, drops
private destinations and IPv6 traffic, and returns no IPv6 address records for guest DNS.
The guest keeps IPv6 loopback.

### Route changes

The saved route is the VM's preference. A UI draft can be inspected before applying it. A
live candidate starts in verification mode and becomes the saved route when all required
checks pass. If the candidate fails, the prior route can regain its grant only while its
gateway session and network path still match. Route changes require a connected VM and
resumed internet access.

### Authorization

The gate states are `closed`, `checking`, and `open`. During verification, the gateway
permits the fixed DNS question and only the shared HTTPS destinations. The guest tries the
alternate HTTPS target when the primary fails. Both targets use the candidate path, and the
guest verifies TLS. The host compares the guest's observed public exit and country with
independent samples from the same route. The check also covers the traffic boundary, time
zone, language, and browser profile. A Chrome page observed live must match the managed
configuration. The installed profile is the durable input to the gate revision.

Static exit policy requires the same public IPv4 address. Dynamic residential policy allows
address rotation within the approved country and continent, with location evidence for the
exact address observed by the guest. The time zone check stays required. When a check fails
only because the VM's time zone differs from a verified exit (and only browser checks that
follow the VM's zone fail with it), the Mac sets the guest to the exit's zone through
`environment_apply` and evaluates the same attempt again. If the exit observation changes,
the new observation must still verify against the same address or region baseline. The
Region section's "Follow exit" switch (on by default, `network_region_follow`) turns this
off. Environment operations run in their own operation lane, so a time zone or language change
never waits behind an apt operation that needs the network. New machines install `locales`
and generate `en_US.UTF-8` during provisioning; other supported locales are generated
offline when applied.

The host can briefly hold guest TCP while it confirms reachability and the verified public
exit. A third failed recheck or a 45-second hold closes internet access. Sleep and stale path
revisions revoke the grant. Automatic recovery retries closed routes with backoff. Checks
retain their evidence and history for the Network Check UI.

## Project sync design

The Mac and guest scan both sides as needed and reconcile against a per-project base. Safe
applies Mac edits while keeping independent guest edits. Two-way reconciles both directions.
Mirror makes the guest match the Mac. Both-side edits in Safe or Two-way become explicit
conflicts. The excluded set is outside the sync plan. Regular files are staged and
published atomically after the payload length and SHA-256 match the planned source and
the destination still matches its scan. The scanner rejects unreadable snapshots before
reconciliation. SHA-256 identifies file contents even when editors preserve length or
mtime. Safe and Two-way keep divergent copies from legacy metadata-only bases as conflicts
until the user selects a version. Matching copies establish a digest-bearing base.
Check source paths on disk before deleting their counterparts. A path filtered out on one
side stays in place, and the rest of the transfer can finish.

FSEvents and inotify trigger scoped batches. A full cycle runs every ten minutes. Short
requests share one SSH transport, scan listings and bases use compact binary forms, and each
direction transfers its batch as a tar stream. The host drains remote diagnostics alongside
scan and transfer streams and retains up to 64 KiB per command for error reporting.
Two-way scans concurrently. Safe skips the guest scan when the Mac matches the base.
Before transfer, the host saves the intended changes so it can recover the base if an
apply reply is lost. An uncertain destination change becomes a conflict. Safe and Two-way
default to a one-time `.git` copy, staged and published as a complete directory. Mirror
includes `.git` in normal sync.

## Agent Client Protocol design

The guest runs one ACP process per live chat over stdin and stdout. Claude Code, Codex, and
Pi use adapters that drive the CLI from Setup. OpenCode and Antigravity run their native ACP
commands. The desktop displays normalized chat state through typed IPC.
[Supported runtimes and account flows](README.md#agents-and-usage)

- **Lifecycle.** Initialize the client and try `session/resume`, then `session/load`, then
  `session/new`. The app keeps its own stored transcript and drops load replay. If the
  saved session cannot reopen, the timeline explains the new session. The native session ID
  is stored in `chat_threads.native_ref`.
- **Controls.** The running agent supplies model, effort, mode, commands, plan, context, and
  steering capability. The guest maps provider-owned permissions to desktop controls. A turn
  can accept `_session/steering` when advertised or take the next message from the queue
  when idle.
- **Interaction and planning.** `session/request_permission` becomes an approval item and
  `elicitation/create` becomes a question. Claude's `ExitPlanMode` waits for a plan
  decision. Codex plans in `collaboration_mode`, Antigravity uses ACP `plan` mode, and
  OpenCode supplies Build and Plan through its own options. Older plan reviews remain
  readable.
- **History and changes.** The guest stores ordered, revisioned items and child snapshots.
  Agent-supplied diffs and Git tree snapshots are separate sources with explicit origins.
  Background tasks link their initiating tool and keep their summaries when complete.

The supplied [OpenCode acceptance test](README.md#opt-in-verification) covers edits,
approvals, steering, restart, and resume. Offline checks cover the other adapters.

Claude's account inspection combines CLI status with local profile and subscription
metadata. Antigravity usage imports completed generations and enriches them with companion
SQLite metadata when available.
[Magpie's Claude account integration](https://github.com/yetone/magpie/blob/177c3fc5069f6f12e0b633b56d35040f7c5b634d/internal/provider/account.go)
·
[CC Switch Antigravity importer](https://github.com/farion1231/cc-switch/blob/v4.0.38/src-tauri/src/services/session_usage_agy.rs)

## Current scope and open work

- **Project sync.** The archive preserves whole-second modification times. Conflicts are
  resolved per file. The UI offers a choice of source version for each conflict.
- **Proxy subscriptions.** Import accepts Clash-style YAML as plain text or base64. The node
  catalog also supports manual entries.
- **Agent configuration.** The shared MCP and Skills pages manage user-level settings.
  Project MCP and OAuth sign-in are provider-managed. OpenCode JSONC is available for
  inspection. The app uses the provider's own CLI config when opening a session.
- **Claude Code process isolation.** Run Cabin-managed Claude Code with an OS-enforced
  boundary that covers the CLI and its descendants. Expose the selected workspace or
  worktree, required tools, and the session's own Claude configuration and authentication.
  Pass an explicit environment allowlist. Keep `cabind` and unrelated guest processes, their
  launch metadata and environments, Cabin's private state and service sockets, other agents'
  credentials, and unselected project directories outside that boundary. Apply the policy to
  ACP chats and Cabin's Claude terminal entry point. Preserve provider permission prompts
  and supported authentication. Verify with stub CLIs that project access works and that
  both the CLI and its subprocesses are denied access to protected data. Use an owner-run
  smoke check for sign-in, chat resume, and terminal use.
- **Desktop rendering.** Sway uses software rendering. GPU-accelerated workloads and a
  separate frame path need performance measurements before a design decision.
- **ACP follow-ups.** Session fork and list, checkpoints, ACP authentication, MCP servers
  passed in `session/new`, and live MCP status are prospective features. A product-level
  goal is a separate design decision from plan review.
- **Provider verification.** Owner-run checks can exercise Claude, Codex, and Pi resume and
  permission mapping and can verify Antigravity sign-in, ACP resume, and usage import
  together on Ubuntu ARM64.
