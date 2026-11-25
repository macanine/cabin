# Native host CLI prototype

The host `cabin` executable forwards the native agent CLI to a Linux `cabind`
worker, not to ACP or SSH. For normal use see [Host CLI](../README.md#host-cli-macos).
The desktop is the sole owner of projects and their sync baselines. It switches
one project to native sync while the CLI holds its local broker connection, then
restores desktop sync. The desktop's existing guest RPC still uses SSH.

The isolated benchmark mode below uses `CABIN_CLI_CHANNEL_DIR` to opt out of the
desktop and keep its own test project registry. Use it only with a disposable
VM and a disposable project, not a desktop-synced project.

- `cabin bind /absolute/guest/project` records a Two-way project for the current
  local directory. The registry is scoped to the private channel directory.
- `cabin claude|codex|agy|opencode|pi [args...]` maps the current directory (also
  under a project root), starts the project watchers, runs an initial sync,
  forwards the CLI, and runs a final sync. The existing sync planner, conflict
  handling, ignore rules, and archive code are reused.
- An interactive terminal uses a guest PTY and raw bytes in both directions;
  a piped invocation preserves stdin EOF, separate stdout and stderr, and the
  child exit code. Arguments are handed to the Linux program directly, not to a
  shell. Paths in arguments are guest paths; credentials and accounts stay in
  the guest. Only one project invocation at a time is supported per channel
  directory.
- The three QEMU serial lanes are `exec` (terminal and process I/O), `sync`
  (`scan`, `pack`, `apply`), and `watch` (guest file notifications). They use
  separate socket chardevs. Payload bytes are framed but are not JSON encoded;
  only launch metadata is JSON. No TCP listener is used by the prototype.

## Run against a disposable test VM

Build both executables on macOS Apple Silicon using the development prerequisites
in the [README](../README.md#development):

```bash
cargo build --locked --manifest-path src-tauri/Cargo.toml --bin cabin
```

```bash
npm run build:agentd
```

Use the native `cabin` binary from `src-tauri/target/debug` (or your
`CARGO_TARGET_DIR`). Use the ARM64 `cabind` built under `core/target` in the VM.

1. Create a short per-VM host directory with mode `0700` (e.g. `mktemp -d
   /tmp/cabin-cli.XXXXXX`). Set `CABIN_CLI_CHANNEL_DIR` to that directory. Boot
   an Ubuntu ARM64 guest under QEMU with `-device virtio-serial-pci`. Add this
   pair of options for each `<lane>` (`exec`, `sync`, `watch`):
   `-chardev socket,id=<lane>,path=<dir>/<lane>.sock,server=on,wait=off` and
   `-device virtserialport,chardev=<lane>,name=org.cabin.<lane>`. A qcow2 overlay
   on the Ubuntu cloud image and a NoCloud seed with a throwaway SSH key are
   sufficient. Bind SSH to loopback for setup/baseline only.
2. Install `cabind` in the unprivileged guest user's `~/.local/bin`. As guest
   root, grant only that user read/write permission to the three
   `/dev/virtio-ports/org.cabin.<lane>` character devices. Start one
   `cabind native-channel <lane> /dev/virtio-ports/org.cabin.<lane>` worker per
   lane as the guest user (not root). For example, use guest `systemd-run` with
   `--uid=bench --gid=bench --setenv=HOME=/home/bench`.
3. For reproducible offline tests, install
   [`native-cli-bench-agent.py`](../scripts/native-cli-bench-agent.py) as
   executable `~/.local/bin/claude` **only in the throwaway VM**. Symlink the
   other four names to it and create `~/baseline`. From a new local test
   directory run `cabin bind /home/bench/project` once.
4. Run the [benchmark harness](../scripts/bench-native-cli.py), passing the host
   binary, local project, and SSH command/options (with the guest host last).
   The benchmark writes `blob` under the guest baseline directory; keep it
   separate from projects. Run `--smoke-only` again after disabling SSH in
   the guest; this verifies terminal and file traffic still work.

## Measured on Apple M5 / Ubuntu 24.04 ARM64, QEMU 11.1.3 with HVF

| Metric | virtio-serial | Reused SSH | Fresh SSH |
| --- | ---: | ---: | ---: |
| Transport + guest process to first byte, median | 17.35 ms | 28.84 ms | 81.80 ms |
| Same, p95 | 19.17 ms | 33.57 ms | 97.50 ms |
| PTY echo with concurrent transfer, p95 | 0.34 ms | 2.97 ms | — |
| Host-to-guest sync payload | 99.23 MiB/s | 115.65 MiB/s | — |
| Guest-to-host sync payload | 232.62 MiB/s | 192.13 MiB/s | — |

The first-byte test uses 20 invocations per mode with the VM already running;
new SSH means a new authenticated connection, not a cold VM. First-byte and
PTY echo timings isolate launch and transport, not initial project reconciliation
or model response time. Throughput is the median of three 64 MiB binary payload
round trips through `cabind sync apply` and `pack`, including the local process
and guest file I/O, not the full project scan. The live end-to-end smoke test
also passed with the guest SSH endpoint unavailable. Real agent logins,
clipboard bridges, desktop coexistence, and Windows are not covered by this
prototype. The short idle retry on disconnected virtio devices and the one-job
lanes are prototype choices, not production multiplexing or service lifecycle.
