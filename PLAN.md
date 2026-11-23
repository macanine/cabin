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
| VM and release inputs | On Apple Silicon, setup downloads the pinned macanine/qemu engine for HVF. Windows x64 runs a native amd64 guest with the installed QEMU, and Linux uses its distribution's QEMU. Setup records the Ubuntu 24.04 or 26.04 LTS image's dated URL and SHA-256. Ubuntu's generic kernel supplies the required 9p and display drivers. |
| Host and guest transport | Tauri commands run on the host. Guest commands use newline-delimited JSON RPC and typed events over `ssh ... cabind connect`. The per-machine SSH key authenticates the relay. The app compares the daemon's hello build hash with its Linux binary for the guest architecture and deploys it when needed. |
| Lifecycle and recovery | The installer collects System, Account, and Review before it starts provisioning. Guest system changes prepare, apply, verify, and roll back through the operation journal. Machine startup reports each phase and keeps its logs. A first-install rollback removes its incomplete disk, while repair uses the existing disk. |
| Desktop and tools | Sway is the guest desktop. noVNC over SSH is the default viewer. SPICE uses the same desktop through a private QEMU socket on supported machines. Node.js, Python, uv, and agent tooling are installed in the guest. |
| UI and data flow | The React window consumes a startup snapshot and typed events. Host and guest report authoritative process and operation state. Visible reads refresh on entry, foreground return, updates, or bounded polling where events are unavailable. Settings use one column with the most relevant state and actions at the top. |
| Project data | Agents work on the VM's local disk. Mac projects sync against a saved base. The VM supports 9p for optional live mounts outside project sync. |

## Network routing and recovery

### Traffic paths

The VM's only network interface connects to the Mac gateway. The route selects direct
transport or a chain of proxy nodes, entry first. Separately, it selects macOS routing or a
physical interface. In bound mode the selected adapter carries direct sockets, the chain's
first hop, and bootstrap DNS. Outbound verification and diagnostic requests use the
candidate or saved route they describe. Guest UDP DNS travels through that route using DNS
over HTTPS. The gateway forwards public IPv4 traffic, drops private destinations and IPv6
traffic, and returns no IPv6 address records for guest DNS. The guest keeps IPv6 loopback.

### Route changes

The saved route is the VM's preference. A UI draft can be inspected before applying it. A
live candidate starts in verification mode and becomes the saved route when all required
checks pass. If the candidate fails, the prior route can regain its grant only while its
gateway session and network path still match. Interface-only changes can be saved while
guest verification is unavailable. Proxy chain changes use the live check.

When the selected adapter is absent at boot, the management relay can start by itself. When
an active adapter changes or disconnects, the gateway drops its old upstream, keeps SSH
available, and closes internet access. Adapter snapshots cover non-default interfaces. On
return, automatic recovery builds a fresh upstream and checks it before admitting ordinary
traffic. The policy keeps a bound route closed during outages, including outages where a
system route is available.

### Authorization

The gate states are `closed`, `checking`, and `open`. During verification, the gateway
permits the fixed DNS question and only the shared HTTPS destinations. The guest tries the
alternate HTTPS target when the primary fails. Both targets use the candidate path, and the
guest verifies TLS. The host compares the guest's observed public exit and country with
independent samples from the same route. The check also covers the traffic boundary, time
zone, language, and browser profile. A Chrome page observed live must match the managed
configuration. The installed profile is the durable input to the gate revision.

Exit samples agree when they share the same address or, for IPv4, the same /24: carrier and
campus NAT pools hand consecutive connections neighbouring addresses. The time zone check
stays required. When a check fails only because the VM's time zone differs from a verified
exit (and only browser checks that follow the VM's zone fail with it), the Mac sets the guest
to the exit's zone through `environment_apply` and evaluates the same attempt again. The
Region section's "Follow exit" switch (on by default, `network_region_follow`) turns this
off. Environment operations run in their own operation lane, so a time zone or language change
never waits behind an apt operation that needs the network. New machines install `locales`
and generate `en_US.UTF-8` during provisioning; other supported locales are generated
offline when applied.

`network_system_route` reports whether macOS sends internet traffic through a tunnel (a VPN
or a TUN proxy such as Clash, recognized by a fake-IP gateway in 198.18.0.0/15). The
interface picker then offers to bind the physical adapter, which bypasses the tunnel.

The Mac can briefly hold guest TCP while it confirms reachability and the verified public
exit. A third failed recheck or a 45-second hold closes internet access. Bound interface
changes, sleep, and stale path revisions revoke the grant. Automatic recovery retries closed
routes with backoff. Checks retain their evidence and history for the Network Check UI.

## Project sync design

The Mac and guest scan both sides as needed and reconcile against a per-project base. Safe
applies Mac edits while keeping independent guest edits. Two-way reconciles both directions.
Mirror makes the guest match the Mac. Both-side edits in Safe or Two-way become explicit
conflicts. The excluded set is outside the sync plan and archive writes use atomic apply.
The scanner rejects unreadable snapshots before reconciliation.

FSEvents and inotify trigger scoped batches. A full cycle runs every ten minutes. Short
requests share one SSH transport, scan listings and bases use compact binary forms, and each
direction transfers its batch as a tar stream. Two-way scans concurrently. Safe skips the
guest scan when the Mac matches the base. Safe and Two-way default to a one-time `.git`
copy. Mirror includes `.git` in normal sync.

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

- **Project sync.** Filesystem clocks are compared at whole-second precision across sides.
  Conflicts are resolved per file. The UI offers a choice of source version for each
  conflict.
- **Proxy subscriptions.** Import accepts Clash-style YAML as plain text or base64. The node
  catalog also supports manual entries.
- **Agent configuration.** The shared MCP and Skills pages manage user-level settings.
  Project MCP and OAuth sign-in are provider-managed. OpenCode JSONC is available for
  inspection. The app uses the provider's own CLI config when opening a session.
- **Desktop rendering.** Sway uses software rendering. GPU-accelerated workloads and a
  separate frame path need performance measurements before a design decision.
- **ACP follow-ups.** Session fork and list, checkpoints, ACP authentication, MCP servers
  passed in `session/new`, and live MCP status are prospective features. A product-level
  goal is a separate design decision from plan review.
- **Provider verification.** Owner-run checks can exercise Claude, Codex, and Pi resume and
  permission mapping and can verify Antigravity sign-in, ACP resume, and usage import
  together on Ubuntu ARM64.
