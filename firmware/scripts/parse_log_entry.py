#!/usr/bin/env python3
"""Parse a base64-encoded log entry and print it in the same format as the
USART log output.

Usage:
    python parse_log_entry.py <base64_entry>

Can also read from stdin (one base64 entry per line):
    sdmmc open logs | python parse_log_entry.py

Wire format:
    [id:4][level:1][task:2][idx:2][time:8][len:2][payload:len]

Packed time (u64 LE):
    bits  0..15  year
    bits 16..23  month
    bits 24..31  day
    bits 32..39  weekday
    bits 40..47  hour
    bits 48..55  minute
    bits 56..63  second
"""

import sys
import struct
import base64

LEVELS = {
    0: "PANIC",
    1: "ERROR",
    2: "WARN",
    3: "INFO",
    4: "DEBUG",
    5: "TRACE",
}

HEADER_SIZE = 19

TASK_NAMES = None


def load_task_names():
    """Load task names from HUBRIS_TASKS env or .work/app.kdl-derived sources."""
    import json
    import os
    global TASK_NAMES

    # Try .work/app.tasks.json first (if the build system generates it)
    for candidate in ["app.kdl"]:
        path = os.path.join(os.path.dirname(__file__), "..", candidate)
        if os.path.exists(path):
            # Parse task names from app.kdl: lines matching 'task "name"'
            names = []
            with open(path) as f:
                for line in f:
                    line = line.strip()
                    if line.startswith("task "):
                        # task "name" {
                        start = line.index('"') + 1
                        end = line.index('"', start)
                        names.append(line[start:end])
            if names:
                TASK_NAMES = names
                return

    TASK_NAMES = []


def task_name(idx: int) -> str:
    if TASK_NAMES is None:
        load_task_names()
    if 0 <= idx < len(TASK_NAMES):
        return TASK_NAMES[idx]
    return f"task{idx}"


def unpack_time(t: int) -> str:
    year = t & 0xFFFF
    month = (t >> 16) & 0xFF
    day = (t >> 24) & 0xFF
    hour = (t >> 40) & 0xFF
    minute = (t >> 48) & 0xFF
    second = (t >> 56) & 0xFF
    if year == 0 and month == 0 and day == 0:
        return "????/??/?? ??:??:??"
    return f"{day:02d}/{month:02d}/{year % 100:02d} {hour:02d}:{minute:02d}:{second:02d}"


def parse_entry(raw: bytes) -> str | None:
    if len(raw) < HEADER_SIZE:
        return None

    level = raw[4]
    task_idx = struct.unpack_from("<H", raw, 5)[0]
    time = struct.unpack_from("<Q", raw, 9)[0]
    data_len = struct.unpack_from("<H", raw, 17)[0]
    payload = raw[HEADER_SIZE:HEADER_SIZE + data_len]

    ts = unpack_time(time)
    lvl = LEVELS.get(level, f"?{level}")
    name = task_name(task_idx)
    text = payload.decode("utf-8", errors="replace")

    return f"{ts} [{lvl} {name}] {text}"


def main():
    if len(sys.argv) > 1:
        # Single entry from argv
        raw = base64.b64decode(sys.argv[-1])
        result = parse_entry(raw)
        if result:
            print(result)
    else:
        # Stream from stdin, one base64 line per entry
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            raw = base64.b64decode(line)
            result = parse_entry(raw)
            if result:
                print(result)


if __name__ == "__main__":
    main()
