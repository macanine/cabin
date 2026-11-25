#!/usr/bin/env python3
"""Compare Cabin native channels with cold and multiplexed SSH on one test VM.

Run only against an isolated VM. See docs/native-cli-prototype.md for setup.
"""

import argparse
import concurrent.futures
import fcntl
import io
import json
import os
import pathlib
import pty
import select
import shlex
import socket
import statistics
import struct
import subprocess
import tarfile
import tempfile
import termios
import time


def frame(sock, kind, data=b""):
    sock.sendall(struct.pack("!BI", kind, len(data)) + data)


def recv(source):
    header = source.read(5)
    if len(header) != 5:
        raise RuntimeError("native channel closed")
    kind, size = struct.unpack("!BI", header)
    if size > 65536:
        raise RuntimeError("invalid frame size")
    body = source.read(size)
    if len(body) != size:
        raise RuntimeError("truncated frame")
    if kind == 10:
        raise RuntimeError(body.decode(errors="replace"))
    return kind, body


def open_exec(directory, root, tty):
    sock = socket.socket(socket.AF_UNIX)
    sock.settimeout(15)
    try:
        sock.connect(str(directory / "exec.sock"))
        source = sock.makefile("rb")
        size = {"cols": 80, "rows": 24, "term": "xterm"} if tty else None
        request = {"version": 1, "job": {"Agent": {"name": "claude", "argv": ["echo" if tty else "first-byte"], "cwd": root, "tty": size}}}
        frame(sock, 1, json.dumps(request).encode())
        assert recv(source) == (6, b"")
        return sock, source
    except BaseException:
        sock.close()
        raise


def native_close(sock, source):
    with sock, source:
        assert recv(source) == (9, b"\x07")
        frame(sock, 5)
        assert recv(source) == (11, b"")


def ssh_start(ssh, command, tty=False):
    opts = ["-tt"] if tty else ["-T"]
    return subprocess.Popen([*ssh[:-1], *opts, ssh[-1], command], stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def first_native(directory, root):
    started = time.perf_counter()
    sock, source = open_exec(directory, root, False)
    assert recv(source) == (7, b"R")
    elapsed = time.perf_counter() - started
    native_close(sock, source)
    return elapsed


def first_ssh(ssh, root):
    started = time.perf_counter()
    with ssh_start(ssh, f"cd {shlex.quote(root)} && ~/.local/bin/claude first-byte") as child:
        assert child.stdout.read(1) == b"R"
        elapsed = time.perf_counter() - started
        assert child.wait(timeout=10) == 7
    return elapsed


def archive(mib):
    data = os.urandom(mib * (1 << 20))
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w") as tar:
        info = tarfile.TarInfo("blob")
        info.size = len(data)
        tar.addfile(info, io.BytesIO(data))
    return b'{"delete":[]}\n' + out.getvalue(), data


def transfer(command, root, payload, data):
    start = time.perf_counter()
    result = subprocess.run(command("apply", root), input=payload, capture_output=True, check=True, timeout=120)
    assert not json.loads(result.stdout).get("failed")
    pushed = time.perf_counter() - start
    start = time.perf_counter()
    result = subprocess.run(command("pack", root), input=b'{"paths":["blob"]}\n', capture_output=True, check=True, timeout=120)
    pulled = time.perf_counter() - start
    with tarfile.open(fileobj=io.BytesIO(result.stdout)) as tar:
        assert tar.extractfile("blob").read() == data
    mib = len(data) / (1 << 20)
    return mib / pushed, mib / pulled


def latency_native(directory, root, parallel):
    sock, source = open_exec(directory, root, True)
    with sock, source:
        assert recv(source) == (7, b"READY\n")
        points = sample_echo(lambda: frame(sock, 2, b"x"), lambda: recv(source)[1], parallel)
        frame(sock, 2, b"!")
        native_close(sock, source)
        return points


def latency_ssh(ssh, root, parallel):
    with ssh_start(ssh, f"cd {shlex.quote(root)} && ~/.local/bin/claude echo", True) as child:
        assert child.stdout.readline().strip() == b"READY"

        def write(char=b"x"):
            child.stdin.write(char)
            child.stdin.flush()

        points = sample_echo(write, lambda: child.stdout.read(1), parallel)
        write(b"!")
        assert child.wait(timeout=10) == 7
        return points


def sample_echo(send, read, parallel):
    points = []
    while not parallel.done():
        start = time.perf_counter()
        send()
        assert read() == b"x"
        points.append(time.perf_counter() - start)
    assert points, "transfer finished before the first sample"
    return points


def wait_for_path(path, child):
    limit = time.monotonic() + 15
    while not path.exists():
        if child.poll() is not None or time.monotonic() > limit:
            raise RuntimeError(f"live sync did not create {path}: {child.stderr.read().decode() if child.poll() is not None else 'timeout'}")
        time.sleep(0.025)


def smoke(cli, project, root):
    def run(argv, **kwargs):
        return subprocess.run([cli, *argv], cwd=project, timeout=30, **kwargs)

    for name in ("claude", "codex", "agy", "opencode", "pi"):
        result = run([name, "literal ; $(false)", "--json"], input=b"piped\n", capture_output=True)
        assert result.returncode == 7, result.stderr
        assert result.stderr == b"stub stderr\n"
        assert json.loads(result.stdout) == {"argv": ["literal ; $(false)", "--json"], "cwd": root, "stdin": "piped\n", "tty": False}
    for name in ("guest-live", "host-live"):
        (project / name).unlink(missing_ok=True)
    with subprocess.Popen([cli, "claude", "watch-test"], cwd=project, stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE) as child:
        wait_for_path(project / "guest-live", child)
        (project / "host-live").write_text("host live change\n")
        _, error = child.communicate(timeout=20)
        assert child.returncode == 0, error
    for disconnect in (False, True):
        master, slave = pty.openpty()
        previous = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 31, 100, 800, 620))
        child = subprocess.Popen([cli, "claude", "tui-test"], cwd=project, stdin=slave,
                                 stdout=slave, stderr=subprocess.PIPE,
                                 env=dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor"))
        received = bytearray()

        def expect(expected):
            limit = time.monotonic() + 15
            while expected not in received and time.monotonic() < limit:
                if select.select([master], [], [], 0.1)[0]:
                    received.extend(os.read(master, 65536))
                elif child.poll() is not None:
                    break
            assert expected in received, bytes(received[-300:])

        try:
            expect(b"\x1b[?1049h\x1b[?2004h\x1b[?1006h\x1b[6nxterm-256color truecolor\nSIZE 31 100 800 620\n")
            controls = b"\x1b[1;1R\x1b[<0;8;9M\x1b[200~" + "测试".encode() + b"\xff\x1b[201~"
            os.write(master, controls)
            expect(controls)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 114, 912, 800))
            expect(b"SIZE 40 114 912 800\n")
            if disconnect:
                child.terminate()
                assert child.wait(timeout=10) == 143, child.stderr.read()
            else:
                os.write(master, b"\x03")
                expect(b"\x1b[?1006l\x1b[?2004l\x1b[?1049l")
                assert child.wait(timeout=10) == 7, child.stderr.read()
            assert termios.tcgetattr(slave) == previous, "host terminal settings were not restored"
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)
            child.stderr.close()
            os.close(master)
            os.close(slave)


def summary(samples):
    return {"median_ms": round(statistics.median(samples) * 1000, 3),
            "p95_ms": round(statistics.quantiles(samples, n=100, method="inclusive")[94] * 1000, 3) if len(samples) > 1 else None,
            "samples": len(samples)}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--cli", required=True)
    ap.add_argument("--project", type=pathlib.Path, required=True)
    ap.add_argument("--guest-project", default="/home/bench/project")
    ap.add_argument("--guest-baseline", default="/home/bench/baseline")
    ap.add_argument("--mib", type=int, default=32)
    ap.add_argument("--runs", type=int, default=15)
    ap.add_argument("--smoke-only", action="store_true")
    ap.add_argument("ssh", nargs=argparse.REMAINDER, help="SSH command and options followed by host")
    args = ap.parse_args()
    directory = pathlib.Path(os.environ["CABIN_CLI_CHANNEL_DIR"])
    smoke(args.cli, args.project, args.guest_project)
    if args.smoke_only:
        print("five argv/pipe aliases, live Two-way sync, PTY controls, resize, and terminal restoration passed")
        return
    ssh = args.ssh[1:] if args.ssh and args.ssh[0] == "--" else args.ssh
    if not ssh:
        ap.error("pass the SSH baseline command after --")
    with tempfile.TemporaryDirectory(prefix="cabin-bench-") as temp:
        mux = str(pathlib.Path(temp) / "mux")
        subprocess.run([*ssh[:-1], "-M", "-S", mux, "-N", "-f", ssh[-1]], check=True, timeout=15)
        try:
            warm = [*ssh[:-1], "-S", mux, ssh[-1]]
            modes = [
                ("native", lambda: first_native(directory, args.guest_baseline),
                 lambda op, root: [args.cli, "__sync", op, root],
                 lambda f: latency_native(directory, args.guest_baseline, f)),
                ("ssh_warm", lambda: first_ssh(warm, args.guest_baseline),
                 lambda op, root: [*warm, f"~/.local/bin/cabind sync {op} {shlex.quote(root)}"],
                 lambda f: latency_ssh(warm, args.guest_baseline, f)),
            ]
            results = {"ssh_cold_start": summary([first_ssh(ssh, args.guest_baseline) for _ in range(args.runs)])}
            payload, data = archive(args.mib)
            for name, start, command, latency in modes:
                starts = [start() for _ in range(args.runs)]
                speeds = []
                points = []
                with concurrent.futures.ThreadPoolExecutor(1) as pool:
                    for _ in range(3):
                        future = pool.submit(transfer, command, args.guest_baseline, payload, data)
                        points.extend(latency(future))
                        speeds.append(future.result())
                results[name] = {"start": summary(starts), "echo_under_transfer": summary(points),
                                 "push_mib_s": round(statistics.median(s[0] for s in speeds), 2),
                                 "pull_mib_s": round(statistics.median(s[1] for s in speeds), 2)}
            print(json.dumps(results, indent=2))
        finally:
            subprocess.run([*ssh[:-1], "-S", mux, "-O", "exit", ssh[-1]], check=False)


if __name__ == "__main__":
    main()
