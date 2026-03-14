#!/usr/bin/env python3
"""
run-log-scraper.py <meta_dir> [--tasks <comma-separated task names>]

Reads log metadata sidecar JSON files from <meta_dir> and emits a collated
JSON array on **stderr** (for nushell `e>|` capture).

When --tasks is provided, a "task_names" entry is included for index→name mapping.
"""

import json
import sys
from collections import OrderedDict
from pathlib import Path


def scrape_sidecars(meta_dir):
    """Read all sidecar JSON files from the metadata directory.

    Returns a bundle dict with types, fields, and species keyed by hash ID.
    """
    types = OrderedDict()
    fields = OrderedDict()
    species = OrderedDict()

    for path in sorted(meta_dir.glob("*.json")):
        try:
            data = json.loads(path.read_text())
        except (json.JSONDecodeError, OSError) as e:
            print(f"warning: failed to read {path}: {e}", file=sys.stderr)
            continue

        entry_id = data.get("id")
        entry = data.get("entry")
        if not entry_id or not entry:
            print(f"warning: malformed sidecar {path.name}", file=sys.stderr)
            continue

        kind = entry.get("kind")
        if kind in ("struct", "enum", "variant"):
            types[entry_id] = entry
        elif kind == "field":
            fields[entry_id] = entry
        elif kind == "species":
            species[entry_id] = entry
        elif kind is not None:
            print(f"warning: unknown kind '{kind}' in {path.name}",
                  file=sys.stderr)

    if not types and not fields and not species:
        return None

    return {
        "types": types,
        "fields": fields,
        "species": species,
    }


def main():
    import argparse

    parser = argparse.ArgumentParser()
    parser.add_argument("meta_dir")
    parser.add_argument("--tasks", default=None,
                        help="Comma-separated task names in index order")
    args = parser.parse_args()

    task_names = []
    if args.tasks:
        task_names = args.tasks.split(",")

    meta_dir = Path(args.meta_dir)
    bundle = scrape_sidecars(meta_dir)

    result = []
    if bundle is not None:
        bundle["task_names"] = task_names
        result.append(bundle)

    # Emit on stderr for nushell `e>|` capture
    print(json.dumps(result), file=sys.stderr)


if __name__ == "__main__":
    main()
