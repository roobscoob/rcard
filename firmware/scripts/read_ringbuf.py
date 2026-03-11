#!/usr/bin/env python3
"""Read a COBS-framed ring buffer partition image and emit base64 NDJSON.

Usage:
    python read_ringbuf.py <partition.bin>

Each output line is a JSON object:
    {"counter": <u32>, "data": "<base64>"}

The data payload for log entries has the wire format:
    [id:4][level:1][task:2][idx:2][time:8][len:2][payload:len]
"""

import sys
import base64
import struct


def cobs_decode(buf: bytes) -> bytes:
    """Decode a COBS-encoded buffer (no leading/trailing zeros)."""
    out = bytearray()
    i = 0
    while i < len(buf):
        code = buf[i]
        i += 1
        if code == 0:
            break
        for _ in range(code - 1):
            if i >= len(buf):
                break
            out.append(buf[i])
            i += 1
        # code < 0xFF means an implicit zero follows (unless end of data)
        if code < 0xFF and i < len(buf):
            out.append(0)
    # remove trailing zero if present (artifact of implicit zero after last group)
    if out and out[-1] == 0:
        out.pop()
    return bytes(out)


def read_messages(data: bytes):
    """Yield (counter, payload) tuples from a ring buffer image."""
    n = len(data)
    pos = 0

    def peek():
        return data[pos] if pos < n else None

    def read_byte():
        nonlocal pos
        if pos >= n:
            return None
        b = data[pos]
        pos += 1
        return b

    def skip_to_null():
        """Skip bytes until we hit a \x00, then consume it."""
        nonlocal pos
        while pos < n and data[pos] != 0:
            pos += 1
        if pos < n:
            pos += 1  # consume the \x00

    def read_cobs_frame():
        """Read COBS bytes until \x00 terminator. Returns the raw COBS bytes."""
        frame = bytearray()
        nonlocal pos
        while pos < n:
            b = data[pos]
            pos += 1
            if b == 0:
                return bytes(frame)
            frame.append(b)
        return bytes(frame)

    # Initial alignment: if first byte is not null, we're mid-message
    b = peek()
    if b is None:
        return
    if b != 0:
        skip_to_null()

    # Now read messages
    while pos < n:
        b = peek()
        if b is None:
            break
        if b == 0:
            pos += 1  # consume leading \x00
            # Check for consecutive nulls (empty region / end of data)
            if pos >= n or peek() == 0:
                continue
            frame = read_cobs_frame()
            if not frame:
                continue
            decoded = cobs_decode(frame)
            if len(decoded) < 4:
                continue
            counter = struct.unpack_from("<I", decoded, 0)[0]
            payload = decoded[4:]
            yield counter, payload
        else:
            # Shouldn't happen after alignment, but be safe
            skip_to_null()


def sort_by_counter(messages: list[tuple[int, bytes]]) -> list[tuple[int, bytes]]:
    """Sort messages by counter, treating wrapping u32 as sequential.

    Finds the discontinuity (where counter jumps backwards) and rotates
    so the oldest message comes first.
    """
    if len(messages) <= 1:
        return messages

    # Find the discontinuity: where counter[i] + 1 != counter[i+1] (mod u32)
    U32 = 0x1_0000_0000
    split = 0
    for i in range(len(messages) - 1):
        expected_next = (messages[i][0] + 1) % U32
        if messages[i + 1][0] != expected_next:
            split = i + 1
            break

    return messages[split:] + messages[:split]


def main():
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <partition.bin>", file=sys.stderr)
        sys.exit(1)

    with open(sys.argv[1], "rb") as f:
        data = f.read()

    messages = sort_by_counter(list(read_messages(data)))

    for _counter, payload in messages:
        print(base64.b64encode(payload).decode("ascii"))


if __name__ == "__main__":
    main()
