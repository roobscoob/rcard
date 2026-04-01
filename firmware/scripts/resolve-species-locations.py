#!/usr/bin/env python3
"""
resolve-species-locations.py <log-metadata.json> <elf_dir>

Post-build step that resolves species log entries to source file:line.

The proc macro emits a `.weak` asm label `__rcard_log_<hash>_<seq>:` at
each log call site.  This script finds those symbols in the task ELFs with
`nm -al`, which reports the source file and line from DWARF debug info.
Updates log-metadata.json in place.
"""

import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

# Match: <addr> <type> <...>__rcard_log_<HASH><suffix> <tab> file:line
# The hash is exactly 16 hex digits; the suffix is _<seq> or Rust mangling.
SYMBOL_RE = re.compile(
    r"[0-9a-fA-F]+\s+\S+\s+\S*__rcard_log_([0-9a-fA-F]{16})\S*\t(.+)"
)


def find_tool(names):
    for name in names:
        if shutil.which(name):
            return name
    return None


def collect_locations(nm_cmd, elf_dir):
    """Run nm -l on each ELF and collect {hash_int: (file, line)}."""
    locations = {}
    for elf_path in sorted(elf_dir.rglob("*")):
        if not elf_path.is_file():
            continue
        try:
            result = subprocess.run(
                [nm_cmd, "-al", str(elf_path)],
                capture_output=True, text=True, timeout=30,
            )
        except (subprocess.TimeoutExpired, OSError):
            continue
        for m in SYMBOL_RE.finditer(result.stdout):
            hash_hex = m.group(1)
            file_line = m.group(2).strip()
            try:
                hash_val = int(hash_hex, 16)
            except ValueError:
                continue
            if "?" in file_line:
                continue
            # Parse "file:line" — careful with Windows drive letters (C:\...)
            parts = file_line.rsplit(":", 1)
            if len(parts) == 2:
                try:
                    file_path = parts[0]
                    line_no = int(parts[1])
                    if hash_val not in locations:
                        locations[hash_val] = (file_path, line_no)
                except ValueError:
                    continue
    return locations


def shorten_path(file_path):
    """Trim an absolute path to a project-relative one."""
    file_path = file_path.replace("\\", "/")
    for marker in ["/firmware/", "/shared/", "/modules/", "/patches/"]:
        idx = file_path.find(marker)
        if idx != -1:
            return file_path[idx + 1:]
    return file_path


def main():
    if len(sys.argv) < 3:
        print(f"usage: {sys.argv[0]} <log-metadata.json> <elf_dir>",
              file=sys.stderr)
        sys.exit(1)

    metadata_path = Path(sys.argv[1])
    elf_dir = Path(sys.argv[2])

    nm_cmd = find_tool(["arm-none-eabi-nm", "rust-nm", "nm"])
    if not nm_cmd:
        print("warning: nm not found, skipping", file=sys.stderr)
        print("0/0", file=sys.stderr)
        return

    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))

    # Build hash -> species entry map
    species_map = {}
    for bundle in metadata:
        for hash_str, species in bundle.get("species", {}).items():
            try:
                species_map[int(hash_str.strip(), 16)] = species
            except ValueError:
                continue

    if not species_map:
        print("0/0", file=sys.stderr)
        return

    locations = collect_locations(nm_cmd, elf_dir)

    resolved = 0
    for hash_val, (file_path, line_no) in locations.items():
        if hash_val in species_map:
            species_map[hash_val]["file"] = shorten_path(file_path)
            species_map[hash_val]["line"] = line_no
            resolved += 1

    metadata_path.write_text(json.dumps(metadata), encoding="utf-8")
    total = len(species_map)
    print(f"{resolved}/{total}", file=sys.stderr)


if __name__ == "__main__":
    main()
