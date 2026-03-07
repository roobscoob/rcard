#!/usr/bin/env python3
"""
KDL preprocessor: resolves named constants defined in `define` blocks,
extracts block-device partition tables and filesystem mappings, and
strips non-hubake sections from the output.

Usage:
    python kdl-preprocess.py app.src.kdl app.kdl

Uses KDL type annotations to mark substitution sites:

    define workgroup {
        kernel 0
        driver 1
        idle 2
    }

    task super {
        priority (workgroup)kernel
    }

Output:

    task super {
        priority 0
    }

Multiple define blocks are supported.

Block-device and filesystem sections are parsed, validated, and written
to a partition table JSON file alongside the output. They are stripped
from the hubake output.
"""

import json
import os
import re
import sys


def parse_defines(text):
    """Extract all `define <name> { key value; ... }` blocks."""
    defines = {}
    pattern = re.compile(
        r'^define\s+([\w-]+)\s*\{([^}]*)\}',
        re.MULTILINE | re.DOTALL,
    )
    for m in pattern.finditer(text):
        name = m.group(1)
        body = m.group(2)
        mapping = {}
        next_value = 0
        for line in body.strip().splitlines():
            line = line.strip()
            if not line:
                continue
            parts = line.rstrip(";").split()
            if len(parts) >= 2:
                mapping[parts[0]] = parts[1]
                next_value = int(parts[1]) + 1
            elif len(parts) == 1:
                mapping[parts[0]] = str(next_value)
                next_value += 1
        defines[name] = mapping
    return defines, pattern


def ensure_supervisor_first(text):
    """Move the task with a `supervisor` flag to be the first task block.

    The `supervisor` line is stripped from the output since hubake doesn't
    understand it — it's purely a preprocessor directive.
    """
    # Match all task blocks (handling nested braces one level deep for git-crate)
    task_pattern = re.compile(
        r'(task\s+[\w-]+\s*\{(?:[^{}]*(?:\{[^{}]*\})?)*[^{}]*\})',
        re.DOTALL,
    )
    tasks = list(task_pattern.finditer(text))
    if not tasks:
        return text

    supervisor_match = None
    for m in tasks:
        if re.search(r'^\s*supervisor\s*$', m.group(0), re.MULTILINE):
            supervisor_match = m
            break

    if supervisor_match is None:
        return text

    # Remove the supervisor line from the block
    cleaned_block = re.sub(
        r'\n\s*supervisor\s*\n', '\n', supervisor_match.group(0),
    )

    # Remove the original supervisor task from the text
    before = text[:supervisor_match.start()]
    after = text[supervisor_match.end():]

    # Find where the first task starts and insert the supervisor there
    first_task = tasks[0]
    if first_task == supervisor_match:
        # Already first — just strip the supervisor line
        return text[:supervisor_match.start()] + cleaned_block + after

    insert_pos = first_task.start()
    return (
        text[:insert_pos]
        + cleaned_block + '\n\n'
        + text[insert_pos:supervisor_match.start()].rstrip()
        + '\n'
        + after.lstrip('\n')
    )


SIZE_MULTIPLIERS = {
    'B': 1,
    'KiB': 1024,
    'MiB': 1024 * 1024,
    'GiB': 1024 * 1024 * 1024,
}

BLOCK_SIZE = 512


def parse_size(size_str, unit_str):
    """Parse a size value like '10 MiB' into bytes."""
    multiplier = SIZE_MULTIPLIERS.get(unit_str)
    if multiplier is None:
        print(f"error: unknown size unit '{unit_str}'", file=sys.stderr)
        sys.exit(1)
    size_bytes = int(size_str) * multiplier
    if size_bytes % BLOCK_SIZE != 0:
        print(
            f"error: size {size_str} {unit_str} ({size_bytes} bytes) "
            f"is not a multiple of block size ({BLOCK_SIZE})",
            file=sys.stderr,
        )
        sys.exit(1)
    return size_bytes


def parse_block_device(text):
    """Extract block-device sections and compute partition tables.

    Returns (partitions_by_device, block_device_pattern) where
    partitions_by_device maps device name -> list of partition dicts.
    """
    devices = {}
    # Match block-device blocks (two levels of nesting)
    pattern = re.compile(
        r'^block-device\s+([\w-]+)\s*\{((?:[^{}]*(?:\{[^{}]*\})?)*[^{}]*)\}',
        re.MULTILINE | re.DOTALL,
    )
    for m in pattern.finditer(text):
        device_name = m.group(1)
        body = m.group(2)
        partitions = []
        offset = 0

        # Match partition blocks within the device
        part_pattern = re.compile(
            r'partition\s+"([\w-]+)"\s*\{([^}]*)\}',
            re.DOTALL,
        )
        for pm in part_pattern.finditer(body):
            part_name = pm.group(1)
            part_body = pm.group(2)

            # Parse size
            size_match = re.search(
                r'size\s+(\d+)\s+(\w+)', part_body,
            )
            if not size_match:
                print(
                    f"error: partition '{part_name}' in device '{device_name}' "
                    f"has no size",
                    file=sys.stderr,
                )
                sys.exit(1)
            size_bytes = parse_size(size_match.group(1), size_match.group(2))

            # Parse format
            fmt_match = re.search(r'format\s+(\w+)', part_body)
            fmt = fmt_match.group(1) if fmt_match else 'raw'

            partitions.append({
                'name': part_name,
                'offset_bytes': offset,
                'offset_blocks': offset // BLOCK_SIZE,
                'size_bytes': size_bytes,
                'size_blocks': size_bytes // BLOCK_SIZE,
                'format': fmt,
            })
            offset += size_bytes

        devices[device_name] = partitions

    return devices, pattern


def parse_filesystems(text):
    """Extract filesystem sections.

    Returns (filesystems, pattern) where filesystems maps
    fs_name -> list of {name, source_device, source_partition} dicts.
    """
    filesystems = {}
    pattern = re.compile(
        r'^filesystem\s+([\w-]+)\s*\{((?:[^{}]*(?:\{[^{}]*\})?)*[^{}]*)\}',
        re.MULTILINE | re.DOTALL,
    )
    for m in pattern.finditer(text):
        fs_name = m.group(1)
        body = m.group(2)
        maps = []

        map_pattern = re.compile(
            r'map\s+([\w-]+)\s*\{([^}]*)\}',
            re.DOTALL,
        )
        for mm in map_pattern.finditer(body):
            map_name = mm.group(1)
            map_body = mm.group(2)
            source_match = re.search(r'source\s+([\w-]+)::([\w-]+)', map_body)
            if not source_match:
                print(
                    f"error: filesystem '{fs_name}' map '{map_name}' "
                    f"has no source",
                    file=sys.stderr,
                )
                sys.exit(1)
            maps.append({
                'name': map_name,
                'source_device': source_match.group(1),
                'source_partition': source_match.group(2),
            })

        filesystems[fs_name] = maps

    return filesystems, pattern


def validate_filesystems(filesystems, devices):
    """Validate that filesystem sources reference littlefs partitions."""
    for fs_name, maps in filesystems.items():
        for m in maps:
            device = devices.get(m['source_device'])
            if device is None:
                print(
                    f"error: filesystem '{fs_name}' map '{m['name']}' "
                    f"references unknown device '{m['source_device']}'",
                    file=sys.stderr,
                )
                sys.exit(1)
            part = next(
                (p for p in device if p['name'] == m['source_partition']),
                None,
            )
            if part is None:
                print(
                    f"error: filesystem '{fs_name}' map '{m['name']}' "
                    f"references unknown partition "
                    f"'{m['source_device']}::{m['source_partition']}'",
                    file=sys.stderr,
                )
                sys.exit(1)
            if part['format'] != 'littlefs':
                print(
                    f"error: filesystem '{fs_name}' map '{m['name']}' "
                    f"sources partition '{m['source_partition']}' which has "
                    f"format '{part['format']}', expected 'littlefs'",
                    file=sys.stderr,
                )
                sys.exit(1)


def parse_uses_partition(text, devices):
    """Extract uses-partition declarations per task and validate them.

    Returns a dict mapping task_name -> [partition_name, ...].
    """
    all_partitions = set()
    for parts in devices.values():
        for p in parts:
            all_partitions.add(p['name'])

    task_pattern = re.compile(
        r'^task\s+([\w_]+)\s*\{((?:[^{}]*(?:\{[^{}]*\})?)*[^{}]*)\}',
        re.MULTILINE | re.DOTALL,
    )
    acl = {}
    for m in task_pattern.finditer(text):
        task_name = m.group(1)
        body = m.group(2)
        partitions = re.findall(r'uses-partition\s+([\w-]+)', body)
        for p in partitions:
            if p not in all_partitions:
                print(
                    f"error: task '{task_name}' uses-partition '{p}' "
                    f"references unknown partition",
                    file=sys.stderr,
                )
                sys.exit(1)
        if partitions:
            acl[task_name] = partitions
    return acl


def inject_storage_uses_task(text, partition_acl):
    """Add uses-task entries to sysmodule_storage for tasks that use partitions.

    This ensures the storage task has SLOTS for every caller that needs
    partition access, enabling runtime ACL checks.
    """
    storage_pattern = re.compile(
        r'(task\s+sysmodule_storage\s*\{)((?:[^{}]*(?:\{[^{}]*\})?)*[^{}]*)\}',
        re.MULTILINE | re.DOTALL,
    )
    m = storage_pattern.search(text)
    if not m:
        return text

    header = m.group(1)
    body = m.group(2)

    # Find existing uses-task declarations
    existing = set(re.findall(r'uses-task\s+([\w_]+)', body))

    # Add missing uses-task entries
    additions = []
    for task_name in sorted(partition_acl.keys()):
        if task_name != 'sysmodule_storage' and task_name not in existing:
            additions.append(f'    uses-task {task_name}')

    if not additions:
        return text

    new_body = body.rstrip() + '\n' + '\n'.join(additions) + '\n'
    return text[:m.start()] + header + new_body + '}' + text[m.end():]


def strip_uses_partition(text):
    """Remove uses-partition lines from task blocks."""
    return re.sub(r'\n\s*uses-partition\s+[\w-]+', '', text)


def resolve(text, defines, define_pattern):
    """Remove define blocks and substitute (type)name annotations with values."""
    out = define_pattern.sub('', text)

    for define_name, mapping in defines.items():
        # Match: (define_name)key — replace the whole annotation+key with the value
        def replacer(m):
            key = m.group(1)
            if key not in mapping:
                print(
                    f"error: '{key}' is not defined in '{define_name}'",
                    file=sys.stderr,
                )
                sys.exit(1)
            return mapping[key]

        out = re.sub(
            rf'\({re.escape(define_name)}\)([\w-]+)',
            replacer,
            out,
        )

    out = ensure_supervisor_first(out)

    # Clean up excessive blank lines left by removed blocks
    out = re.sub(r'\n{3,}', '\n\n', out)
    return out.strip() + '\n'


def main():
    if len(sys.argv) < 3:
        print(f"usage: {sys.argv[0]} <input.kdl> <output.kdl>", file=sys.stderr)
        sys.exit(1)

    with open(sys.argv[1]) as f:
        text = f.read()

    # Parse and validate block-device / filesystem sections
    devices, bd_pattern = parse_block_device(text)
    filesystems, fs_pattern = parse_filesystems(text)

    partition_acl = {}
    if devices:
        partition_acl = parse_uses_partition(text, devices)
    if filesystems:
        validate_filesystems(filesystems, devices)

    # Inject uses-task entries into sysmodule_storage for ACL enforcement
    text = inject_storage_uses_task(text, partition_acl)

    # Strip block-device, filesystem, and uses-partition from output
    text_for_hubake = bd_pattern.sub('', text)
    text_for_hubake = fs_pattern.sub('', text_for_hubake)
    text_for_hubake = strip_uses_partition(text_for_hubake)

    # Resolve defines and reorder supervisor
    defines, define_pattern = parse_defines(text_for_hubake)
    output = resolve(text_for_hubake, defines, define_pattern) if defines else text_for_hubake

    os.makedirs(os.path.dirname(sys.argv[2]), exist_ok=True)
    with open(sys.argv[2], 'w') as f:
        f.write(output)

    # Write partition table JSON alongside the output
    if devices or filesystems:
        json_path = sys.argv[2].rsplit('.', 1)[0] + '.partitions.json'
        partition_data = {
            'block_size': BLOCK_SIZE,
            'devices': devices,
            'filesystems': {
                name: maps for name, maps in filesystems.items()
            },
            'partition_acl': partition_acl,
        }
        with open(json_path, 'w') as f:
            json.dump(partition_data, f, indent=2)
        print(f"Partition table written to {json_path}", file=sys.stderr)


if __name__ == '__main__':
    main()
