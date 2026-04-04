# /// script
# dependencies = ["pywinpty; sys_platform == 'win32'"]
# ///
"""
run-clippy.py <cargo_args>...

Runs cargo clippy through a PTY so output streams to the terminal in real time.
If clippy exits 0, all its output is erased and the caller sees a clean line.
If it fails, the output stays visible.

Prints the elapsed time (seconds) to stderr on success so the caller can use it.
Exit code mirrors cargo's.
"""

import importlib.util
import io
import sys
import time
from pathlib import Path

# Import launch_pty from run-exhubris.py (hyphen in name, so use importlib)
_spec = importlib.util.spec_from_file_location(
    "run_exhubris", Path(__file__).parent / "run-exhubris.py"
)
_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_mod)
launch_pty = _mod.launch_pty

def main():
    argv = ["cargo"] + sys.argv[1:]

    stream, wait = launch_pty(argv)
    text = io.TextIOWrapper(
        io.BufferedReader(stream, buffer_size=256),
        encoding="utf-8",
        errors="replace",
        newline="",
    )

    newline_count = 0
    t0 = time.monotonic()

    try:
        for raw in text:
            sys.stdout.write(raw)
            sys.stdout.flush()
            newline_count += raw.count("\n")
    finally:
        stream.close()

    code = wait()
    elapsed = time.monotonic() - t0

    if code == 0:
        # Erase all cargo output
        for _ in range(newline_count):
            sys.stdout.write("\033[F\033[K")
        sys.stdout.flush()
        # Report elapsed time on stderr for the caller
        print(f"{elapsed:.2f}", file=sys.stderr)

    sys.exit(code)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
