#!/usr/bin/env python3
"""
run-exhubris.py <app_kdl> <zip>

Runs: hubake build <app_kdl> -o <zip> through a PTY so the subprocess
sees isatty()=True and emits full formatted output.

stdout+stderr are merged (PTY limitation) and parsed inline.

Platform support:
  Unix    - stdlib `pty`, no extra deps
  Windows - requires `pywinpty`:  pip install pywinpty
"""

import io
import json
import os
import re
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent
IS_WINDOWS = sys.platform == "win32"


# ---------------------------------------------------------------------------
# Cross-platform PTY launch
# Returns (raw_stream: RawIOBase, wait: () -> int)
# ---------------------------------------------------------------------------

def _launch_unix(argv: list[str]):
    import pty, fcntl, termios

    master_fd, slave_fd = pty.openpty()

    # Mirror host terminal size so tools querying TIOCGWINSZ get sane values
    try:
        ts = fcntl.ioctl(sys.stdout.fileno(), termios.TIOCGWINSZ, b'\x00' * 8)
        fcntl.ioctl(master_fd, termios.TIOCSWINSZ, ts)
    except Exception:
        pass

    proc = subprocess.Popen(
        argv,
        stdin=slave_fd,
        stdout=slave_fd,
        stderr=slave_fd,
        close_fds=True,
    )
    os.close(slave_fd)

    stream = io.open(master_fd, "rb", buffering=0, closefd=True)

    def wait():
        proc.wait()
        return proc.returncode

    return stream, wait


def _launch_windows(argv: list[str]):
    try:
        import winpty
    except ImportError:
        print("ERROR: pywinpty is required on Windows.  pip install pywinpty", file=sys.stderr)
        sys.exit(1)

    proc = winpty.PtyProcess.spawn(argv)

    class _WinStream(io.RawIOBase):
        def readable(self):
            return True

        def readinto(self, b):
            try:
                data = proc.read(len(b))
                if not data:
                    return 0
                if isinstance(data, str):
                    data = data.encode("utf-8", errors="replace")
                n = len(data)
                b[:n] = data
                return n
            except EOFError:
                return 0

    def wait():
        proc.wait()
        return proc.exitstatus

    return _WinStream(), wait


def launch_pty(argv: list[str]):
    return _launch_windows(argv) if IS_WINDOWS else _launch_unix(argv)


# ---------------------------------------------------------------------------
# Parser
# ---------------------------------------------------------------------------

def _parse_kv_block(lines):
    rec = {}
    last_key = None
    for line in lines:
        m = re.match(r'│\s*(.*?)\s*│\s*(.*?)\s*│\s*$', line)
        if not m:
            continue
        key, val = m.group(1), m.group(2)
        if key:
            rec[key] = val
            last_key = key
        elif last_key:
            rec[last_key] = rec[last_key] + "\n" + val
    return rec


def _parse_msg_block(lines):
    parts = []
    for line in lines:
        m = re.match(r'│\s*(.*?)\s*│\s*$', line)
        if m:
            parts.append(m.group(1))
    return " ".join(parts).strip()


# Matches an allocations data row.
# START and END are optional (absent for (total) rows).
_ALLOC_ROW = re.compile(
    r'^\s*(\S+)\s+'                   # memory region  e.g. "flash"
    r'(\S+)\s+'                        # task name      e.g. "fob" or "(total)"
    r'(?:(0x[0-9a-fA-F]+)\s+'         # start addr (optional)
    r'(0x[0-9a-fA-F]+)\s+)?'          # end addr   (optional)
    r'([\d.]+ \w+|0 bytes)\s+'        # size       e.g. "24.5 KiB"
    r'([\d.]+ \w+|\d+ bytes)'         # lost
)

_ALLOC_HEADER = re.compile(r'^\s*MEMORY\s+TASK\s+START')
_ALLOC_START  = re.compile(r'^Allocations\s*\(')


def _parse_alloc_line(line):
    m = _ALLOC_ROW.match(line)
    if not m:
        return None
    return {
        "memory": m.group(1),
        "task":   m.group(2),
        "start":  m.group(3),
        "end":    m.group(4),
        "size":   m.group(5),
        "lost":   m.group(6),
    }


def _flush_alloc(buf):
    """Parse and emit an allocations block from buffered lines."""
    rows = []
    for l in buf[1:]:   # buf[0] is the "Allocations (...)" header line
        r = _parse_alloc_line(l)
        if r:
            rows.append(r)
    return rows or None


def parse_stream(stream: io.RawIOBase):
    """
    Generator: yields parsed event dicts from a merged PTY byte stream.

    Event types:
      {"type": "table",       "data": {str: str}}
      {"type": "message",     "text": str}
      {"type": "allocations", "data": [row_dict, ...]}
      {"type": "raw",         "text": str}   <- unrecognised / noise lines
    """
    text = io.TextIOWrapper(
        io.BufferedReader(stream, buffer_size=256),
        encoding='utf-8',
        errors='replace',
        newline='',     # don't mangle \r\n from PTY
    )

    mode = "idle"
    buf  = []

    for raw in text:
        line = raw.rstrip("\n")   # PTYs emit \n

        if mode == "idle":
            if line.startswith("╭"):
                mode = "kv" if "┬" in line else "msg"
                buf = [line]
            elif _ALLOC_START.match(line):
                mode = "alloc"
                buf = [line]
            else:
                yield {"type": "raw", "text": raw}

        elif mode in ("kv", "msg"):
            buf.append(line)
            if line.startswith("╰"):
                if mode == "kv":
                    yield {"type": "table", "data": _parse_kv_block(buf)}
                else:
                    yield {"type": "message", "text": _parse_msg_block(buf)}
                mode = "idle"
                buf = []

        elif mode == "alloc":
            if _ALLOC_HEADER.match(line):
                continue    # skip column header row

            if line.strip() == "":
                rows = _flush_alloc(buf)
                if rows:
                    yield {"type": "allocations", "data": rows}
                mode = "idle"
                buf = []
            else:
                buf.append(line)

    # EOF — flush any pending alloc block (no trailing blank in some builds)
    if mode == "alloc" and len(buf) > 1:
        rows = _flush_alloc(buf)
        if rows:
            yield {"type": "allocations", "data": rows}


# ---------------------------------------------------------------------------
# Handlers
# ---------------------------------------------------------------------------

newline_count = 0
allocations = {}
time = ""
in_progress = None

def log_in_progress(name):
    print(f"  \033[33m⟳\033[0m {name}")

def log_done(name, time, padding=30):
    print(f"  \033[32m✓\033[0m \033[1m{name.ljust(padding)}\033[0m \033[2m{time}\033[0m")

def handle_event(v):
    global newline_count
    global allocations
    global time
    global in_progress
    if v["type"] == "table":
        data = v["data"]
        if "App name" in data:
            return
        if "Product" in data:
            # clear the last newline_count lines to overwrite the "Building product: ..." line
            for _ in range(newline_count):
                sys.stdout.write("\033[F\033[K")  # move cursor up and clear line
            sys.stdout.flush()

            if in_progress:
                sys.stdout.write("\033[F\033[K")  # clear the previous in-progress line
                log_done(in_progress, time)

            log_in_progress(data['Product'])
            in_progress = data['Product']
            newline_count = 0
            return
        print(f"Unhandled table output: {json.dumps(data)}")

    elif v["type"] == "allocations":
        allocations = v["data"]

    elif v["type"] == "message":
        pass    # unhelpful, skip

    elif v["type"] == "raw":
        text = v["text"]
        if text and text != "\r" and text != "\r\n" and not text.startswith("warning: adjusted region ram base up to "):
            sys.stdout.write(text)
            newline_count += text.count("\n")
            m = re.search(r"target\(s\) in (\d+(?:\.\d+)?)s", text)
            if m:
                time = m.group(1)
        sys.stdout.flush()


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    global allocations
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <app_kdl> <zip>", file=sys.stderr)
        sys.exit(1)

    app_kdl, zip_out = sys.argv[1], sys.argv[2]

    stream, wait = launch_pty(["hubake", "build", app_kdl, "-o", zip_out])

    try:
        for event in parse_stream(stream):
            handle_event(event)
    finally:
        stream.close()

    code = wait()

    if code == 0:
        for _ in range(newline_count):
            sys.stdout.write("\033[F\033[K")  # move cursor up and clear line
        sys.stdout.flush()

        if in_progress:
            sys.stdout.write("\033[F\033[K")  # clear the previous in-progress line
            log_done(in_progress, time)
        rows = [e for e in allocations if e.get("task") != "(total)"]
        print(json.dumps(rows), file=sys.stderr)

    sys.exit(code)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass