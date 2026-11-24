# Working on Cabin

The Tauri 2 desktop app runs one Ubuntu VM through QEMU. Apple Silicon uses ARM64 and
HVF, Windows x64 uses amd64 and WHPX or TCG, and Linux development uses distro QEMU.
The five agent CLIs run inside the VM, where `cabind` owns sessions, chats, workspaces,
and setup.

Use [README.md](README.md) for product behavior, setup, and test commands. Use
[PLAN.md](PLAN.md) for architecture decisions and open work. This file maps implementation
entry points and records contracts that affect changes. Keep project documentation in
English and extend these files for cross-project changes. Package documentation belongs in
its existing README, such as [ip-detection/README.md](ip-detection/README.md).

## Implementation index

| Area | Start here | Related code and tests |
|---|---|---|
| Host, VM, and SSH | [host/](src-tauri/src/host/mod.rs), [machine.rs](vm/src/machine.rs), [agentd.rs](vm/src/agentd.rs) | `src-tauri/src/host/{machine,guest,network_check}.rs`, `src-tauri/src/machine_service.rs`, `vm/src/{config,vm,os,qemu_engine,platform}.rs`, `vm/src/steps/` |
| Installer and startup | [InstallWizard.tsx](src/views/InstallWizard.tsx), [StartReportView.tsx](src/components/StartReportView.tsx) | `src/views/install/{InstallSetup,InstallProgress,MachineCard}.tsx`, `src/components/{Boot,MachineGate,Recovery}.tsx`, `src/lib/{installForm,wizard,startReport,recovery}.ts` |
| IPC and application state | [common.ts](src/services/ipc/common.ts), [dispatch.rs](src-tauri/src/host/dispatch.rs), [rpc.rs](core/src/rpc.rs) | `src/services/ipc/`, `src/state/{store,reducer}.ts`, `core/src/model.rs`, `src-tauri/src/lib.rs` |
| Routes, interfaces, and subscriptions | [NetworkDashboard.tsx](src/components/network/NetworkDashboard.tsx), [global_network/](src-tauri/src/global_network/mod.rs) | `src-tauri/src/global_network/{gate,paths,health,catalog}.rs`, `src/lib/networkRoute.ts`, `src/components/network/useNetworkLive.ts`, `vm/src/network_interfaces.rs`, `vm/src/netgw/`, `core/src/proxy/`, `tests/geo.test.mjs`, `vm/tests/gateway_interface_live.rs` |
| Network checks and exit | [network_gate/](src-tauri/src/network_gate/), [network_health.rs](src-tauri/src/network_health.rs), [network_probe.rs](core/src/network_probe.rs) | `src-tauri/src/network_gate/{report,exit,browser}.rs`, `vm/src/network_probe_targets.rs`, `src/components/network/{NetworkGatePanel,NetworkCheckModal}.tsx`, `src/state/networkMonitor.ts`, `src/lib/networkSummary.ts`, `ip-detection/` |
| Desktop and browser | [ChromeView.tsx](src/views/ChromeView.tsx), [chrome_feature.rs](vm/src/chrome_feature.rs) | `src/components/{BrowserEnvironmentPanel,ChromeViewer}.tsx`, `src/lib/browserEnvironment.ts`, `core/src/{browser,native_display}/`, `core/src/{guest_desktop,device_probe,guest_dns}.rs` |
| App shell and window | [App.tsx](src/App.tsx), [TitleBar.tsx](src/components/TitleBar.tsx), [window_chrome.rs](src-tauri/src/window_chrome.rs) | `src/components/Sidebar.tsx`, `src/lib/windowChrome.ts`, `src/styles/shell.css`, `src-tauri/tauri.macos.conf.json` |
| Chat runtime and rendering | [agent_runtime/](core/src/agent_runtime/), [Chat.tsx](src/components/Chat.tsx) | `core/src/{chat_view,workspace_diff}.rs`, `src/components/chat/`, `src/lib/{chatTimeline,chatTasks}.ts`, `tests/chat-timeline.test.mjs` |
| Agent setup and accounts | [AgentsView.tsx](src/views/AgentsView.tsx), [agents/](core/src/agents/) | `src/lib/{agentsHub,agentLimits}.ts`, `src/components/{PaceMeter,AccountLimits}.tsx`, `core/src/agents/claude/account.rs`, `core/src/agent_runtime/{acp/launch,runtimes}.rs` |
| MCP, skills, and mentions | [McpView.tsx](src/views/McpView.tsx), [SkillsView.tsx](src/views/SkillsView.tsx) | `core/src/agents/{mcp_library,skill_library,skill_sources}.rs`, `agent_catalog`, `workspace_files`, `src/components/chat/useComposerMenu.ts` |
| Usage and performance | [UsageView.tsx](src/views/UsageView.tsx), [usage/](usage/), [MonitorPanel.tsx](src/components/MonitorPanel.tsx) | `src/components/usage/{UsageMonitor,usageParts}.tsx`, `src/lib/{usageView,perfGauges}.ts` |
| Project sync and Mac permissions | [sync.rs](src-tauri/src/sync.rs), [core/src/sync/](core/src/sync/), [host_permissions.rs](src-tauri/src/host_permissions.rs) | `src/components/NewWorkspaceModal.tsx`, `src/components/sync/`, `src/lib/syncForm.ts`, `src/views/settings/PermissionsSettings.tsx` |
| Workbench and worktrees | [WorkbenchPanel.tsx](src/components/workbench/WorkbenchPanel.tsx), [core/src/workbench/](core/src/workbench/), [worktree.rs](core/src/worktree.rs) | `src/lib/workbench.ts`, `vm/src/preview_forward.rs`, `tests/workbench.test.mjs` |
| Terminals and diffs | [Terminal.tsx](src/components/Terminal.tsx), [Diff.tsx](src/components/diff/Diff.tsx) | `src/lib/{terminalFit,terminalTheme,terminalPaste,patch}.ts`, `vite.config.ts`, `src/styles/{session,diff}.css` |

## Host, guest, and IPC contracts

- Call Tauri through `src/services/ipc/`. `invokeGuest(method, params)` calls
  `guest_invoke`. `Host::call` handles host-owned methods, including `network_*` and
  `proxy_*`, and forwards guest methods over SSH to `core/src/rpc.rs::dispatch`. Keep
  method names and params aligned with `GuestMethod` in `src/services/ipc/common.ts`. Keep
  Tauri commands thin and put SSH and QEMU mechanics in `vm/`. Long Mac operations report
  through `Channel<TaskEvent<T>>`.
- Save VM resource requests separately from the running config. Under the machine operation
  lock, a cold start can grow the standalone qcow2, verify its virtual capacity, and save the
  new allocation. The pending disk request remains until the root-owned guest helper verifies
  the enlarged ext4 filesystem. Never shrink an image or filesystem to roll back an expansion.
- Use camelCase wire fields and preserve stable `{ code, message, details? }` errors.
  Regenerate [bindings](README.md#ipc-bindings) from Rust and leave
  `src/services/ipc/generated/` to ts-rs. Render backend state and events. `useApp`
  selectors can derive presentation values.
- Store and transmit Unix milliseconds. Normalize external epochs in Rust with
  `util::normalize_unix_millis`. Format guest time through `src/lib/time.ts` (`guestDate`
  and `formatGuestDateTime` ), using the clock offset from `src/state/clockSync.ts`. Format
  Mac and absolute server timestamps directly in the Mac's zone. Accept legacy ISO
  timestamps when reading VM step state, logs, and installer metadata.
- `vm/src/os.rs` owns the Ubuntu LTS releases, mirrors, and kernel packages. Save the dated
  image URL and SHA-256 selected at install. Legacy Debian configurations report that they
  need an Ubuntu reinstall. On macOS `vm/src/qemu_engine.rs` prefers bundled QEMU and
  supports downloading the pinned `macanine/qemu` asset for unbundled development. Keep
  `RELEASE` and `SHA256` in sync with `scripts/vendor.json`. Linux development uses distro
  QEMU.
- The `cabind` hello carries the running executable's SHA-256 as `build`.
  `Host::bundled_build` caches the bundle hash by size and mtime, and `rpc::check_hello`
  determines whether to deploy. A missing daemon or build mismatch triggers one deploy and
  reconnect. First-install steps and system services use the root-owned helper. Update the
  app package, Tauri config, and guest crate version together.
  `app_and_guest_versions_agree` checks them.
- Apply guest configuration and system changes through `ops::OperationManager` (prepare,
  apply, verify, rollback). Use argv for external commands and `system::privileged` (`sudo
  -n`) for guest root commands. Fixed SSH command strings live in `vm/src/agentd.rs`
  alongside the existing VM step scripts. Edit agent configuration in place with a backup
  under `<data>/backups/agents/<agent>/`.

## Networking and desktop

- `GlobalNetwork` owns the saved `NetworkRoute`, proxy catalog, gateway, and grant. Keep
  `bind_interface` independent of the proxy chain's `enabled` flag. Scope route checks, node
  test keys, bootstrap DNS, and the first outbound hop to the selected adapter.
  [Routing behavior and recovery](PLAN.md#network-routing-and-recovery)
- `network_gate::region_follow_zone` decides when a failed check may set the guest's time
  zone to the verified exit's; `Host::follow_exit_region` applies it and re-probes within the
  same attempt. Never relax a required check to work around an environment mismatch.
- `Host::apply_network_route` saves interface-only changes when live verification is
  unavailable. `finish_check` commits a verified candidate, and `restore_gate` uses a
  rollback point tied to the gateway session and path revision. `poll_interface_paths`
  invalidates stale adapter bindings and triggers recovery while the SSH relay remains
  available.
- `vm/src/network_interfaces.rs` uses `netdev` on every host for index, link state, IPv4
  addresses, and display labels. Compare interface index and usable IPv4 addresses
  independently of default-path events. Keep the probe and packet allowlist aligned through
  `vm/src/network_probe_targets.rs`.
- Observe the exit through the gateway listener. `ip-detection` follows
  [its documented provider order](ip-detection/README.md#provider-order). Publish the
  address first, cache the classification by IP, and discard observations when the path
  identity changes. Sample visible gateway traffic once a second and retain up to 30 samples
  per route. The Network Check lists blockers and their fixes first and folds passed
  checks. Its timeline shows observed exit IP and environment changes.
- Sway uses pixman. noVNC connects to the guest-loopback `wayvnc` through SSH. SPICE adds a
  virtio GPU and a private QEMU socket on machines without the device profile. Both viewers
  share a Sway session. `cabind native-display` manages display transactions and
  recovery. `desktop::launch` opens each GUI app in its own Sway workspace. Managed browser,
  device, desktop, and proxy DNS helpers run in guest Rust. `core/src/python.rs` manages
  Python and uv for user workspaces.
- `BrowserEnvironmentPanel` edits region and identity and is the destination of browser
  fixes in Network Check. Apply region changes live with `sync_environment`. Apply identity
  changes in place and restart Chrome with `chrome_feature::apply_profile`. Record a live
  change through `steps::browser::record_live_change` so the next start sees the updated
  fingerprint. `ChromeViewer` retries quickly at first and then every eight seconds. Fit to
  window follows the viewer continuously on Wayland and applies once on other backends.
- Gate revision uses the installed browser profile and the VM's region. `browserLive` is
  required when a page was observed. Closed, sleeping, idle, or uninspectable Chrome
  produces a skipped live check. Live readings are outside the gate revision.
  `browser_needs_follow` compares the actual region before requesting a sync.

## Chats, agents, and usage

- Guest Rust owns ACP. `acp/view.rs` translates agent updates into `ChatItem`,
  `ChatControls`, and subagent snapshots. `ChatContext` is the write path, `session.rs`
  owns one live chat, and `Store` assigns `revision`, `ordinal`, and timestamps.
  `ChatControls` is live state delivered in `chatControls` events and `chat_snapshot`. The
  UI uses `agent_chat_capabilities` and renders the supplied options, excluding options
  marked `hidden` for the permission picker.
- Keep provider launch arguments, environment variables, and mode mappings in
  `acp/launch.rs`. `runtimes.rs` owns npm packages, pins, and side-by-side versions under
  `<data>/acp/<agent>/<version>`. Claude, Codex, and Pi adapters drive the CLI from Setup
  and install with `--omit=optional`. OpenCode's platform binary is an optional dependency
  of its own package. Antigravity runs `agy acp` from Setup and supplies model choices live.
  The guest splits ungrouped `provider/model` choices into provider and model ID in
  `acp/view.rs` and `discover.rs`.
- `chat_steer` delivers `_session/steering` and `chat_set_option` changes live options.
  Continue in a fresh native session with a timeline notice when reopening fails. Approval
  decisions use `accept`, `acceptAlways`, `decline`, `declineAlways`, or `cancel`.
  Append `:<optionId>` when an agent offers several options of one kind. Claude's
  `ExitPlanMode` is a real plan approval with the adapter's own choices. Codex uses
  `collaboration_mode` and Antigravity uses native `plan` mode. Keep `chat_plans` readable
  for chats recorded before ACP. `/goal` is a separate product decision from ACP planning.
- Persist child snapshots and items through `chat_subagents`. Sources include ACP
  `subagent_update`, OpenCode's `_meta["opencode/child-session"]`, and the child ID from a
  subagent tool. Map ACP `async_task_*` to `ChatAsyncTask` and set its `itemId` to the
  initiating tool so the strip can jump to it. Keep finished tasks and their summaries
  visible.
- Render agent diffs and `workspace_diff` in `FileChanges` with an explicit origin. The
  workspace diff snapshots Git trees through a temporary index and contributes paths missing
  from structured agent output. The guest supplies `action`, `subject`, and `files`
  through `chat_view.rs`. `src/lib/chatTimeline.ts` owns turn folding and tool grouping.
  Add chat behavior to the relevant `components/chat/` hook and keep `Chat.tsx` focused on
  layout.
- Each agent's CLI, config, MCP codec, skill directories, and `skill_insert` belong in
  `core/src/agents/`. `mcp_library.rs` manages user-level server bindings using the codec's
  supported transports. `skill_library.rs` manages shared installs and updates, and
  `skill_sources.rs` reads trees and bounded archives. `agent_catalog` loads composer skills
  from disk. `workspace_files` supplies `@` mentions through a bounded walk that skips
  hidden, dependency, and build trees.
- Codex account snapshots live under `<data>/codex-accounts`, with separate `0600`
  credential files. Expose credentials through explicit import or export only. Create
  exports as new `0600` files and allow account switching while Codex is idle.
  `codex login status` determines login status, and Codex app-server supplies limits and
  reset cards. Keep native `auth.json`, CLIProxyAPI, and Sub2API formats supported.
- Claude account inspection uses `claude auth status --json` and local profile metadata.
  Expose display fields and distinguish temporary unavailability from signed-out state. Read
  limits through `/usage`, serialized per account with a check of the identified seat
  before and after the read. Antigravity's account status uses the CLI's cached sign-in
  state. The CLI owns sign-in and verification through its terminal.
- Usage imports native records and stores totals and prices in SQLite. The Antigravity
  importer reads completed generations and enriches them with model and timing data from its
  native database. `UsageMonitor` shares refresh and daily spend across the start page,
  sidebar, and Usage. Price rules in `usage/` cover context bands, UTC time-of-day and date
  multipliers, and user overrides.

## Project files and terminals

- Project sync runs `scan`, `pack`, `apply`, and `watch` through `cabind sync` on
  the guest. `SyncManager` drives both sides and stores projects and binary bases under
  `<home>/sync`. Reuse the SSH transport for short commands. Two-way scans both sides in
  parallel. Safe skips the guest scan when the Mac matches the base. Ignore rules include
  `.gitignore`, `.cabinignore`, configured patterns, and unticked paths. The scanner and
  watcher share rules for Cabin's temporary files. Watcher scopes use `/` on every host,
  and explicit watcher rescan signals request a full cycle.
- Keep both versions of a conflict until `sync_resolve` selects one. Publish regular
  files atomically after checking the staged payload length and preserve ignored paths.
  Reject writes through symlinked directories and
  abort unreadable scans before planning. Windows Two-way scans both complete trees before
  planning, stops on path aliases, and checks on disk before treating a missing local entry
  as a deletion or a new pull destination. Mirror syncs `.git` with the project. Other modes
  default to `copyOnce` so each side can commit independently. Mac permissions inspect
  selected folder access at launch and on foreground return. Two-way needs write access. An
  explicit access request can trigger a protected read and the macOS prompt.
- Workbench uses the session's worktree when present and the workspace otherwise. `fs_list`
  and `fs_read` validate paths before and after symlink resolution. `git_status` uses
  porcelain v2 with `GIT_OPTIONAL_LOCKS=0`. Preview forwards the guest user's loopback
  listener through `ssh -N -L` and displays it in an iframe. The file tree uses
  `@headless-tree/react` and `@react-symbols/icons`. Code uses Shiki through `CodeView`.
- Plain terminals use `agent: null` (`'terminal'` in storage). The ACP terminal gets raw
  output, and its tool card uses `util::clean_terminal_text`. Reset pooled ghostty-web
  emulators with RIS (`\x1bc`) after `open()` and suppress emulator-generated `onData`
  during replay. `terminalTheme.ts` sets colors and recoloring rebuilds the emulator.
  `terminalAppearance.ts` owns the bundled font, text-size preference, and scrollbar gutter.
  `terminalFit.ts` fits per frame and resizes the PTY when settled. Keep the 1.2 line height
  and check the canvas patch in `vite.config.ts` on ghostty-web upgrades.
  `terminal_paste_image` saves attachments in the guest. Paste the returned path with
  bracketed-paste markers when requested by the terminal program.

## UI conventions

- Use Tailwind CSS 4 utilities for component-local styling. `src/styles/base.css` owns
  colors, the `--space-*` and `--text-*` scales, glass utilities, and shared motion.
  `src/App.css` defines cascade order so utilities can override component styles. Tailwind
  runs without preflight. Keep structural Mantine, Streamdown, terminal, and diff styles in
  CSS. Keep keyframe names unique. `src/theme.ts` owns Mantine's palette, radii, and short
  fades. Unstyled inputs get their focus treatment from the wrapping control.
- Use short page fades, steady idle rows, and motion for opening, confirmation, and active
  progress. Respect Reduce Motion and Lite mode. Views rise in with `.view-enter`, lists
  that open with a view use `.stagger`, a moving selection glides with `--ease-spring` and
  `--motion-glide` (the sidebar indicator, segmented controls), and a running task uses
  `.live-dot`. `StartReportView` uses the shared `Boot` parts, shows begun steps with a
  bounded stagger, and keeps detailed logs folded until failure. The installer keeps a
  two-phase layout for setup and live progress and reuses the `Boot` step and log controls.
  Use `StepProgress`, `LogView`, and `RecoveryPanel` for progress, logs, and recovery actions.
- AppKit owns macOS traffic-light controls. `window_chrome` reads their geometry on the main
  thread. The web toolbar follows that geometry, reserves space before the first
  measurement, and uses color feedback with stable layout. Keep sidebar vibrancy and the
  rounded content card. Use `ViewHeader` (title, quiet subtitle, actions; a Back button as
  `lead`, never a logo) for view headers and `TitleBarContent` only for custom ones such as
  the session breadcrumb. Sidebar rows share one 20px icon column, so labels align at one x.
- Put the primary state or action first. Use `SettingsPage` and `Section` for settings. The
  start page shows live facts, the composer, Continue, and Projects. Use `AgentLogo`,
  `AgentSelect`, `AgentChips`, and `AgentChoice` for gray agent marks. Show a mark only where it is
  the subject (the agent's own row or detail, matrix column heads, the agent picker); rows
  about tasks, usage, or models name the agent in text. `AgentChips` is a text control. `ModelPicker` and
  `PermissionPicker` share `ComposerChip` and `Tile`. Group models by provider and show
  search for long lists. The permission picker confirms full access.
- Read through backend events, entry and foreground refresh, and mutation invalidation.
  `useAutoRefresh` coalesces foreground updates and bounds visible polling. Keep the last
  result during a background refresh. Reserve buttons for actions such as granting access,
  checking a draft, or restarting a service.
- Agents leads with Plan limits (one tile per provider window) above one row per CLI with
  compact limit meters, direct actions, and inline Setup. Providers shows each CLI's native
  account and limit controls. Every plan window renders through `PaceMeter` with
  `forecastLimit` from `src/lib/agentLimits.ts`: used share, the pace-projected share at
  reset, and a tick for the time passed. Word forecasts as pace estimates and withhold them
  early in a window or without a reset time. Usage leads with filters,
  figures, and trend, followed by tables. The sidebar includes today's spend,
  the network status, and the CPU, RAM, swap, and load-per-core rings. Poll the busiest
  processes only while the performance details are open.
- Use `src/lib/format.ts` for counts and prices in English. Use bundled SVGs in `Flag.tsx`
  with ISO codes from `geo.ts`. For long operations, keep a single loading toast and update
  its ID through `uiActions.updateToast`. `Toasts` pauses timers on hover and releases
  image object URLs when dismissed.
- Render agent Markdown through `components/chat/Timeline.tsx` using Streamdown. Style its
  parts in `styles/markdown.css` using `data-streamdown`. `Diff.tsx` wraps
  `@git-diff-view/react` for inline and side-by-side diffs, word highlights, wrapping, and
  copying. `src/lib/patch.ts::toHunk` converts a unified patch for the component. Keep chat
  layouts and disclosures in their components.

## Host platforms

- `vm/src/arch.rs` (`GuestArch`) is the single source of architecture decisions: QEMU
  binary, firmware names, machine type, image suffix, mirrors, Chrome package, and browser
  platform. `vm/src/accel.rs` owns acceleration status and the WHPX probe and TCG fallback.
  Do not branch on `aarch64` or `x86_64` elsewhere.
- macOS (Apple Silicon) runs an arm64 guest on HVF with
  the AppKit window chrome. Windows x64 runs an amd64 guest on WHPX. Both bundle their
  QEMU (and Windows its OpenSSH) under `vendor/` (`vm/src/vendor.rs`). Linux development
  uses distro QEMU (KVM or TCG). One code path serves every host: `chrono` for time,
  `getrandom`, `fs4` for volumes, `sysinfo` for process checks, termination, and memory,
  `which` for tools, `netdev` for adapters, `sha-crypt` for the guest password hash, std
  file locks, and the in-process seed ISO writer (`vm/src/iso.rs`). Every host uses the
  same transport (`vm/src/sockets.rs`, `vm/src/transport.rs`): QMP, the guest agent, the
  serial console, and the gateway `-netdev stream` are AF_UNIX sockets at the instance
  paths (`uds_windows` supplies the Windows `UnixStream`/`UnixListener`), and the gateway
  reaches the shoes stack through a connected loopback UDP pair waited on with `polling`
  (see `third_party/shoes/VENDORED.txt`). Use `Readiness::scope` to unregister watched
  sockets before releasing their borrows. The gateway owns and joins workers even if
  startup fails partway through. Keep socket paths within about 104 bytes. The few
  real differences (Unix mode bits, `setsid`, the effective-owner check) live in
  `util::chmod` and `platform.rs`. The guest side of `core` stays Unix-only: the
  host links only `hello`, `model`, `sync` (host half), `store`, `proxy`, `ipv4`, `fsx`,
  `error`, `paths`, and `util`; `agent_core::hello` carries the handshake the shell
  compares builds with. Windows app data lives under `%APPDATA%\cabin`, WebView2 data
  under `%LOCALAPPDATA%`, and OpenSSH ControlMaster is off. Keep new host code
  platform-neutral or cfg-gated; never let guest-only modules grow host dependencies.

## Verification

- Use the [development commands](README.md#development) for frontend, Chromium, and Rust
  tests. Use temp directories, in-memory SQLite, loopback listeners, and
  `system::fake::FakeSystem` for unit tests. `src-tauri/src/sync.rs` also has end-to-end
  tests using `CABIND_TEST_BIN` or a locally built `cabind` with a local shell
  transport.
- Use real OpenCode v2 runs for live ACP checks and replay captures. Fixtures under
  `core/src/agent_runtime/acp/fixtures/` preserve recorded provider events and normalize the
  workspace path. Use offline checks for Claude Code, Codex, Pi, and Antigravity. Adapter
  handshake tests use stub CLIs. Keep Claude Code and the Claude Agent SDK out of automated
  runs.
- Reapply the changes in [VENDORED.txt](third_party/shoes/VENDORED.txt) when upgrading shoes.
