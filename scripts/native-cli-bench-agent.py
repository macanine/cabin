#!/usr/bin/python3
"""Offline guest CLI fixture. Install as claude only in a disposable VM."""
import fcntl
import json
import os
import signal
import struct
import termios
import pathlib
import sys
import time
import tty

if sys.argv[1:] == ['first-byte']:
    os.write(1, b'R')
    sys.exit(7)
if sys.argv[1:] == ['echo']:
    tty.setraw(0)
    os.write(1, b'READY\n')
    while char := os.read(0, 1):
        if char == b'!':
            break
        os.write(1, char)
    sys.exit(7)
if sys.argv[1:] == ['tui-test']:
    enter = b'\x1b[?1049h\x1b[?2004h\x1b[?1006h\x1b[6n'
    leave = b'\x1b[?1006l\x1b[?2004l\x1b[?1049l'

    def report_size(*_):
        rows, cols, width, height = struct.unpack('HHHH', fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8)))
        os.write(1, f'SIZE {rows} {cols} {width} {height}\n'.encode())

    tty.setraw(0)
    signal.signal(signal.SIGWINCH, report_size)
    os.write(1, enter + f"{os.environ.get('TERM')} {os.environ.get('COLORTERM')}\n".encode())
    report_size()
    while char := os.read(0, 64):
        if char == b'\x03':
            os.write(1, leave)
            sys.exit(7)
        os.write(1, char)
    sys.exit(9)
if sys.argv[1:] == ['watch-test']:
    time.sleep(0.5)
    pathlib.Path('guest-live').write_text('guest change\n')
    limit = time.monotonic() + 15
    while not pathlib.Path('host-live').exists():
        if time.monotonic() > limit:
            sys.exit(9)
        time.sleep(0.05)
    sys.exit(0)
print(json.dumps(dict(argv=sys.argv[1:], cwd=os.getcwd(), stdin=sys.stdin.read(), tty=os.isatty(0))))
print('stub stderr', file=sys.stderr)
sys.exit(7)
