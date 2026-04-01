#!/usr/bin/env python3
"""
KDL preprocessor: resolves named constants defined in `define` blocks,
extracts block-device partition tables and filesystem mappings, and
strips non-hubake sections from the output.

Usage:
    python kdl-preprocess.py app.src.kdl app.kdl

Uses KDL type annotations to mark substitution sites:

    define "workgroup" {
        kernel
        driver 1
        idle 2
    }

    task "super" {
        priority (workgroup)"kernel"
    }

Output:

    task super {
        priority 0
    }

Multiple define blocks are supported.

Size type annotations are resolved to byte values:

    stack-size (KiB)8    ->  stack-size 8192
    size (MiB)10         ->  size 10485760

Block-device and filesystem sections are parsed, validated, and written
to a partition table JSON file alongside the output. They are stripped
from the hubake output.

Task dependency directives:
    uses-sysmodule X      -> uses-task sysmodule_X
    peer-sysmodule X      -> uses-task sysmodule_X  (peers.json; bind macro will error)
    uses-partition X      -> (implicit) uses-task sysmodule_storage
    uses-notification X   -> (implicit) uses-task sysmodule_reactor
    unsafe-uses-task X    -> uses-task X  (bypass for non-sysmodule deps)
    unsafe-uses-task *    -> uses-task for every other task
    uses-task X           -> ERROR (use uses-sysmodule or unsafe-uses-task)


Memory allocations:
    board { memory "region" { base 0x...; size 0x...; } }
    allocation "name" { in "region"; size (MiB)1; align 0x100; policy { auto; } }
    allocation "name" { in "region"; size (MiB)1; policy { fixed_to 0x...; } }
    task X { uses-allocation "name" { read; write; } }

    Allocations are resolved to concrete addresses (fixed or auto first-fit,
    largest-first), injected as peripherals in the chip KDL, and
    uses-allocation is transformed to uses-peripheral.

Conditional compilation:
    include-if "feature(NAME)"   -> node is kept only if NAME is in --feature list

    Supported on: task, block-device/partition, filesystem/map,
    and notification group children. Nodes without include-if are always kept.
"""

import hashlib
import json
import os
import re
import sys
from pathlib import Path

import kdl

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

SIZE_MULTIPLIERS = {
    'B': 1,
    'KiB': 1024,
    'MiB': 1024 * 1024,
    'GiB': 1024 * 1024 * 1024,
    'KB': 1000,
    'MB': 1000 * 1000,
    'GB': 1000 * 1000 * 1000,
    'Kbit': 125,
    'Mbit': 1000 * 125,
    'Gbit': 1000 * 1000 * 125,
}

DEFAULT_BLOCK_SIZE = 512

PARSE_CONFIG = kdl.ParseConfig(
    nativeUntaggedValues=False,
    nativeTaggedValues=False,
)

PRINT_CONFIG = kdl.PrintConfig(indent='    ')

# ---------------------------------------------------------------------------
# Feature gate expression evaluator
# ---------------------------------------------------------------------------

# Grammar (standard precedence: && binds tighter than ||):
#   expr     = and_expr ( "||" and_expr )*
#   and_expr = atom ( "&&" atom )*
#   atom     = "feature(" NAME ")" | "(" expr ")"

_FEATURE_RE = re.compile(r'feature\(\s*([A-Za-z_][A-Za-z0-9_-]*)\s*\)')


def _tokenize_feature_expr(s):
    """Tokenize a feature expression into a list of tokens."""
    tokens = []
    i = 0
    while i < len(s):
        if s[i].isspace():
            i += 1
        elif s[i:i+2] in ('&&', '||'):
            tokens.append(s[i:i+2])
            i += 2
        elif s[i] == '(':
            # Check if this is feature(...)
            m = _FEATURE_RE.match(s, i)
            if m:
                tokens.append(('feature', m.group(1)))
                i = m.end()
            else:
                tokens.append('(')
                i += 1
        elif s[i] == ')':
            tokens.append(')')
            i += 1
        else:
            die(f"unexpected character '{s[i]}' in include-if expression: {s!r}")
    return tokens


def _parse_feature_expr(tokens, pos=0):
    """Parse an or-expression. Returns (ast, next_pos)."""
    left, pos = _parse_and_expr(tokens, pos)
    while pos < len(tokens) and tokens[pos] == '||':
        right, pos = _parse_and_expr(tokens, pos + 1)
        left = ('or', left, right)
    return left, pos


def _parse_and_expr(tokens, pos):
    """Parse an and-expression."""
    left, pos = _parse_atom(tokens, pos)
    while pos < len(tokens) and tokens[pos] == '&&':
        right, pos = _parse_atom(tokens, pos + 1)
        left = ('and', left, right)
    return left, pos


def _parse_atom(tokens, pos):
    """Parse an atom: feature(NAME) or ( expr )."""
    if pos >= len(tokens):
        die("unexpected end of include-if expression")
    tok = tokens[pos]
    if isinstance(tok, tuple) and tok[0] == 'feature':
        return tok, pos + 1
    if tok == '(':
        node, pos = _parse_feature_expr(tokens, pos + 1)
        if pos >= len(tokens) or tokens[pos] != ')':
            die("missing ')' in include-if expression")
        return node, pos + 1
    die(f"unexpected token {tok!r} in include-if expression")


def eval_feature_expr(expr_str, enabled_features):
    """Evaluate an include-if expression string against a set of enabled features."""
    tokens = _tokenize_feature_expr(expr_str)
    if not tokens:
        die(f"empty include-if expression")
    ast, pos = _parse_feature_expr(tokens)
    if pos != len(tokens):
        die(f"trailing tokens in include-if expression: {expr_str!r}")

    def evaluate(node):
        if isinstance(node, tuple) and node[0] == 'feature':
            return node[1] in enabled_features
        op, left, right = node
        if op == 'and':
            return evaluate(left) and evaluate(right)
        if op == 'or':
            return evaluate(left) or evaluate(right)
        die(f"unknown AST node: {node!r}")

    return evaluate(ast)


def apply_feature_gates(doc, enabled_features):
    """Remove nodes gated by include-if whose condition is not met.

    Processes: top-level task nodes, block-device/partition children,
    filesystem/map children, and notifications group children.
    """
    def is_included(node):
        """Check if a node's include-if (if any) passes."""
        cond = find_child(node, 'include-if')
        if cond is None:
            return True
        expr_str = node_arg(cond)
        if expr_str is None:
            die(f"include-if in '{node.name}' has no expression argument")
        return eval_feature_expr(expr_str, enabled_features)

    def strip_include_if(node):
        """Remove include-if child directives from a node."""
        node.nodes = [c for c in node.nodes if c.name != 'include-if']

    def gate_block_device_children(node):
        """Gate partition children within a block-device node."""
        if node.name == 'block-device':
            node.nodes = [
                c for c in node.nodes
                if c.name != 'partition' or is_included(c)
            ]
            for c in node.nodes:
                if c.name == 'partition':
                    strip_include_if(c)

    # Gate top-level task nodes
    kept_nodes = []
    for node in doc.nodes:
        if node.name == 'task':
            if not is_included(node):
                continue
            strip_include_if(node)
        elif node.name == 'block-device':
            gate_block_device_children(node)
        elif node.name == 'board':
            # Gate block-device partition children inside board
            for child in node.nodes:
                gate_block_device_children(child)
        elif node.name == 'filesystem':
            # Gate map children
            node.nodes = [
                c for c in node.nodes
                if c.name != 'map' or is_included(c)
            ]
            for c in node.nodes:
                if c.name == 'map':
                    strip_include_if(c)
        elif node.name == 'notifications':
            # Gate notification group children
            node.nodes = [
                c for c in node.nodes
                if is_included(c)
            ]
            for c in node.nodes:
                strip_include_if(c)
        kept_nodes.append(node)
    doc.nodes = kept_nodes


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def die(msg):
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def node_arg(node, index=0):
    """Get the string value of a positional argument."""
    if index >= len(node.args):
        return None
    val = node.args[index]
    return val.value if hasattr(val, 'value') else val


def node_name_arg(node):
    """Get the first positional argument as a string (the common 'name' pattern)."""
    return node_arg(node, 0)


def find_children(node, name):
    """Yield child nodes matching a given name."""
    for child in node.nodes:
        if child.name == name:
            yield child


def find_child(node, name):
    """Return the first child node matching a given name, or None."""
    return next(find_children(node, name), None)


def task_name(task_node):
    """Get the task name from a task node."""
    return node_name_arg(task_node)


def find_tasks(doc):
    """Yield all task nodes from the document."""
    return find_children(doc, 'task')


def collect_task_names(doc):
    """Return a list of all task names in declaration order."""
    return [task_name(t) for t in find_tasks(doc)]


# ---------------------------------------------------------------------------
# Size resolution
# ---------------------------------------------------------------------------


def resolve_size_value(val):
    """If val has a size unit tag (B, KiB, MiB, GiB), return the byte count.
    Otherwise return None."""
    if not hasattr(val, 'tag') or val.tag not in SIZE_MULTIPLIERS:
        return None
    raw = val.value
    num = float(raw) if isinstance(raw, str) else raw
    multiplier = SIZE_MULTIPLIERS[val.tag]
    return int(num) * multiplier


def parse_size_from_node(node, context, block_size=None):
    """Extract byte size from a node's first argument (must have a size tag)."""
    if not node.args:
        die(f"{context} has no size")
    result = resolve_size_value(node.args[0])
    if result is None:
        die(f"{context} size must have a unit tag ({', '.join(SIZE_MULTIPLIERS)})")
    if block_size is not None and result % block_size != 0:
        die(
            f"{context} size ({result} bytes) "
            f"is not a multiple of block size ({block_size})"
        )
    return result


def resolve_size_annotations(node):
    """Recursively resolve size unit tags to plain integer byte values."""
    new_args = []
    for arg in node.args:
        byte_val = resolve_size_value(arg)
        if byte_val is not None:
            new_args.append(kdl.Decimal(mantissa=byte_val, exponent=0, tag=None))
        else:
            new_args.append(arg)
    node.args = new_args
    for child in node.nodes:
        resolve_size_annotations(child)


# ---------------------------------------------------------------------------
# Define resolution
# ---------------------------------------------------------------------------


def parse_defines(doc):
    """Extract all `define "name" { key; key value; ... }` blocks.

    Returns a dict mapping define_name -> {key: value_str}.
    """
    defines = {}
    for node in find_children(doc, 'define'):
        name = node_name_arg(node)
        mapping = {}
        next_value = 0
        for child in node.nodes:
            if child.args:
                val = node_arg(child, 0)
                num = float(val) if isinstance(val, str) else val
                mapping[child.name] = int(num)
                next_value = int(num) + 1
            else:
                mapping[child.name] = next_value
                next_value += 1
        defines[name] = mapping
    return defines


def resolve_defines(node, defines):
    """Recursively replace (define_name)"key" annotations with integer values."""
    new_args = []
    for arg in node.args:
        if hasattr(arg, 'tag') and arg.tag in defines:
            mapping = defines[arg.tag]
            key = arg.value
            if key not in mapping:
                die(f"'{key}' is not defined in '{arg.tag}'")
            new_args.append(
                kdl.Decimal(mantissa=mapping[key], exponent=0, tag=None)
            )
        else:
            new_args.append(arg)
    node.args = new_args
    for child in node.nodes:
        resolve_defines(child, defines)


# ---------------------------------------------------------------------------
# Block device / partition parsing
# ---------------------------------------------------------------------------


def parse_block_devices(doc):
    """Extract block-device sections and compute partition tables.

    Block-device nodes are looked up inside the board block first,
    then at the top level for backwards compatibility.

    Returns a dict mapping device_name -> device dict with keys:
        size, mapping, partitions, ftab (optional).
    """
    board_node = next(find_children(doc, 'board'), None)
    source = board_node if board_node is not None else doc
    devices = {}
    for bd_node in find_children(source, 'block-device'):
        device_name = node_name_arg(bd_node)

        block_size_node = find_child(bd_node, 'block-size')
        block_size = parse_size_from_node(
            block_size_node, f"block-device '{device_name}' block-size",
        ) if block_size_node is not None else DEFAULT_BLOCK_SIZE

        dev_size_node = find_child(bd_node, 'size')
        dev_size = parse_size_from_node(
            dev_size_node, f"block-device '{device_name}'",
            block_size=block_size,
        ) if dev_size_node is not None else None

        mapping_node = find_child(bd_node, 'mapping')
        mapping = None
        if mapping_node is not None:
            raw = node_arg(mapping_node)
            mapping = int(float(raw)) if isinstance(raw, str) else int(raw)

        partitions = []
        offset = 0
        for part_node in find_children(bd_node, 'partition'):
            part_name = node_name_arg(part_node)
            context = f"partition '{part_name}' in device '{device_name}'"

            size_node = find_child(part_node, 'size')
            if size_node is None:
                die(f"{context} has no size")
            size_bytes = parse_size_from_node(size_node, context, block_size=block_size)

            fmt_node = find_child(part_node, 'format')
            fmt = node_arg(fmt_node) if fmt_node else 'raw'

            # Apply alignment (pad offset up to next multiple)
            align_node = find_child(part_node, 'align')
            if align_node is not None:
                align_bytes = parse_size_from_node(align_node, context)
                if align_bytes & (align_bytes - 1) != 0:
                    die(f"{context} align must be a power of two")
                remainder = offset % align_bytes
                if remainder != 0:
                    offset += align_bytes - remainder

            # Ensure partition offset is block-aligned
            if offset % block_size != 0:
                die(
                    f"{context} offset ({offset} bytes) "
                    f"is not a multiple of block size ({block_size})"
                )

            part_dict = {
                'name': part_name,
                'offset_bytes': offset,
                'size_bytes': size_bytes,
                'format': fmt,
            }

            source_node = find_child(part_node, 'source')
            if source_node is not None:
                part_dict['source'] = node_arg(source_node)

            if fmt == 'ftab':
                bt_node = find_child(part_node, 'boot_target')
                if bt_node is None:
                    die(f"{context} has format 'ftab' but no boot_target")
                part_dict['boot_target'] = node_arg(bt_node)

            partitions.append(part_dict)
            offset += size_bytes

        if dev_size is not None and offset > dev_size:
            die(
                f"block-device '{device_name}': partitions total "
                f"{offset} bytes but device size is {dev_size} bytes"
            )

        # Validate ftab partitions
        ftab_parts = [p for p in partitions if p['format'] == 'ftab']
        ftab_info = None
        if ftab_parts:
            if len(ftab_parts) > 1:
                die(
                    f"block-device '{device_name}': "
                    f"multiple ftab partitions found; only one is allowed"
                )
            ftab_part = ftab_parts[0]
            if ftab_part['offset_bytes'] != 0:
                die(
                    f"block-device '{device_name}': "
                    f"ftab partition '{ftab_part['name']}' must be the "
                    f"first partition (offset 0)"
                )
            if mapping is None:
                die(
                    f"block-device '{device_name}': "
                    f"ftab partition requires a 'mapping' address on the "
                    f"block-device"
                )
            boot_target_name = ftab_part['boot_target']
            boot_target = next(
                (p for p in partitions if p['name'] == boot_target_name),
                None,
            )
            if boot_target is None:
                die(
                    f"block-device '{device_name}': ftab boot_target "
                    f"'{boot_target_name}' references unknown partition"
                )
            ftab_info = {
                'ftab_addr': mapping,
                'ftab_partition': ftab_part['name'],
                'boot_target': boot_target_name,
                'boot_target_addr': mapping + boot_target['offset_bytes'],
                'boot_target_size': boot_target['size_bytes'],
            }

        devices[device_name] = {
            'size': dev_size,
            'block_size': block_size,
            'mapping': mapping,
            'partitions': partitions,
            'ftab': ftab_info,
        }
    return devices


# ---------------------------------------------------------------------------
# Filesystem parsing
# ---------------------------------------------------------------------------


def parse_filesystems(doc):
    """Extract filesystem sections.

    Returns a dict mapping fs_name -> list of
    {name, source_device, source_partition} dicts.
    """
    filesystems = {}
    for fs_node in find_children(doc, 'filesystem'):
        fs_name = node_name_arg(fs_node)
        maps = []
        for map_node in find_children(fs_node, 'map'):
            map_name = node_name_arg(map_node)
            source_node = find_child(map_node, 'source')
            if source_node is None:
                die(f"filesystem '{fs_name}' map '{map_name}' has no source")
            source_str = node_arg(source_node)
            if '::' not in source_str:
                die(
                    f"filesystem '{fs_name}' map '{map_name}' source "
                    f"'{source_str}' must be 'device::partition'"
                )
            dev, part = source_str.split('::', 1)
            maps.append({
                'name': map_name,
                'source_device': dev,
                'source_partition': part,
            })
        filesystems[fs_name] = maps
    return filesystems


def validate_filesystems(filesystems, devices):
    """Validate that filesystem sources reference littlefs partitions."""
    for fs_name, maps in filesystems.items():
        for m in maps:
            dev = devices.get(m['source_device'])
            if dev is None:
                die(
                    f"filesystem '{fs_name}' map '{m['name']}' "
                    f"references unknown device '{m['source_device']}'"
                )
            part = next(
                (p for p in dev['partitions'] if p['name'] == m['source_partition']),
                None,
            )
            if part is None:
                die(
                    f"filesystem '{fs_name}' map '{m['name']}' "
                    f"references unknown partition "
                    f"'{m['source_device']}::{m['source_partition']}'"
                )
            if part['format'] != 'littlefs':
                die(
                    f"filesystem '{fs_name}' map '{m['name']}' "
                    f"sources partition '{m['source_partition']}' which has "
                    f"format '{part['format']}', expected 'littlefs'"
                )


# ---------------------------------------------------------------------------
# Partition ACL
# ---------------------------------------------------------------------------


def parse_uses_partition(doc, devices):
    """Extract uses-partition declarations per task and validate them.

    Returns a dict mapping task_name -> [partition_name, ...].
    """
    all_partitions = set()
    for dev in devices.values():
        for p in dev['partitions']:
            all_partitions.add(p['name'])

    acl = {}
    for task_node in find_tasks(doc):
        tname = task_name(task_node)
        partitions = [
            node_arg(c) for c in find_children(task_node, 'uses-partition')
        ]
        for p in partitions:
            if p not in all_partitions:
                die(
                    f"task '{tname}' uses-partition '{p}' "
                    f"references unknown partition"
                )
        if partitions:
            acl[tname] = partitions
    return acl


# ---------------------------------------------------------------------------
# Notifications
# ---------------------------------------------------------------------------


def parse_notifications(doc):
    """Extract notification group definitions.

    Returns a dict mapping group_name -> {min_priority, max_priority}.
    """
    groups = {}
    for notif_node in find_children(doc, 'notifications'):
        for group_node in notif_node.nodes:
            name = group_node.name

            min_node = find_child(group_node, 'min-priority')
            max_node = find_child(group_node, 'max-priority')
            if min_node is None or max_node is None:
                die(
                    f"notification group '{name}' must have "
                    f"min-priority and max-priority"
                )

            min_p = int(float(node_arg(min_node)))
            max_p = int(float(node_arg(max_node)))
            if min_p > max_p:
                die(
                    f"notification group '{name}' has "
                    f"min-priority ({min_p}) > max-priority ({max_p})"
                )
            groups[name] = {
                'min_priority': min_p,
                'max_priority': max_p,
            }
    return groups


def parse_notification_acl(doc, notification_groups):
    """Extract pushes-notification and uses-notification from tasks.

    Returns (pushers, subscribers) where each maps
    task_name -> [group_name, ...].
    """
    pushers = {}
    subscribers = {}
    for task_node in find_tasks(doc):
        tname = task_name(task_node)

        pushed = [
            node_arg(c)
            for c in find_children(task_node, 'pushes-notification')
        ]
        for g in pushed:
            if g not in notification_groups:
                die(
                    f"task '{tname}' pushes-notification '{g}' "
                    f"which is not defined"
                )
        if pushed:
            pushers[tname] = pushed

        used = [
            node_arg(c)
            for c in find_children(task_node, 'uses-notification')
        ]
        for g in used:
            if g not in notification_groups:
                die(
                    f"task '{tname}' uses-notification '{g}' "
                    f"which is not defined"
                )
        if used:
            subscribers[tname] = used

    return pushers, subscribers


# ---------------------------------------------------------------------------
# Allocations
# ---------------------------------------------------------------------------


def parse_board_memory(doc):
    """Extract memory regions from board blocks.

    Returns a dict mapping region_name -> {base, size}.
    """
    regions = {}
    for board_node in find_children(doc, 'board'):
        for mem_node in find_children(board_node, 'memory'):
            name = node_name_arg(mem_node)
            base_node = find_child(mem_node, 'base')
            size_node = find_child(mem_node, 'size')
            if base_node is None or size_node is None:
                die(f"memory region '{name}' must have base and size")
            base = int(float(node_arg(base_node)))
            size = int(float(node_arg(size_node)))
            if name in regions:
                die(f"duplicate memory region '{name}'")
            regions[name] = {'base': base, 'size': size}
    return regions


def parse_allocations(doc, memory_regions):
    """Extract allocation blocks.

    Returns a dict mapping alloc_name -> {region, size, align, policy, policy_arg}.
    """
    allocations = {}
    for alloc_node in find_children(doc, 'allocation'):
        name = node_name_arg(alloc_node)

        in_node = find_child(alloc_node, 'in')
        if in_node is None:
            die(f"allocation '{name}' has no 'in' (memory region)")
        region = node_arg(in_node)
        if region not in memory_regions:
            die(f"allocation '{name}' references unknown memory region '{region}'")

        size_node = find_child(alloc_node, 'size')
        if size_node is None:
            die(f"allocation '{name}' has no size")
        size_val = resolve_size_value(size_node.args[0]) if size_node.args else None
        if size_val is None:
            size_val = int(float(node_arg(size_node)))
        if size_val <= 0:
            die(f"allocation '{name}' has invalid size {size_val}")

        align_node = find_child(alloc_node, 'align')
        align = 1
        if align_node is not None:
            align = int(float(node_arg(align_node)))
            if align <= 0 or (align & (align - 1)) != 0:
                die(f"allocation '{name}' alignment {align} must be a power of 2")

        policy_node = find_child(alloc_node, 'policy')
        if policy_node is None:
            die(f"allocation '{name}' has no policy")

        policy = None
        policy_arg = None
        for child in policy_node.nodes:
            if policy is not None:
                die(f"allocation '{name}' has multiple policy directives")
            if child.name == 'auto':
                policy = 'auto'
            elif child.name == 'fixed_to':
                policy = 'fixed'
                policy_arg = int(float(node_arg(child)))
            else:
                die(f"allocation '{name}' has unknown policy '{child.name}'")

        if policy is None:
            die(f"allocation '{name}' has empty policy")

        if name in allocations:
            die(f"duplicate allocation '{name}'")

        allocations[name] = {
            'region': region,
            'size': size_val,
            'align': align,
            'policy': policy,
            'policy_arg': policy_arg,
        }
    return allocations


def solve_allocations(allocations, memory_regions):
    """Solve allocation addresses. Returns a dict mapping name -> {base, size, region}.

    Fixed allocations are placed first, then auto allocations are placed
    largest-first using first-fit to minimize fragmentation.
    """
    # Group by region
    by_region = {}
    for name, alloc in allocations.items():
        by_region.setdefault(alloc['region'], []).append((name, alloc))

    resolved = {}

    for region_name, allocs in by_region.items():
        region = memory_regions[region_name]
        region_base = region['base']
        region_end = region_base + region['size']

        # Place fixed allocations first
        placed = []  # (base, size, name)
        auto_allocs = []

        for name, alloc in allocs:
            if alloc['policy'] == 'fixed':
                base = alloc['policy_arg']
                size = alloc['size']
                align = alloc['align']

                if align > 1 and base % align != 0:
                    die(
                        f"allocation '{name}' fixed_to {base:#x} "
                        f"is not aligned to {align:#x}"
                    )
                if base < region_base or base + size > region_end:
                    die(
                        f"allocation '{name}' fixed_to {base:#x} size {size:#x} "
                        f"does not fit in region '{region_name}' "
                        f"({region_base:#x}..{region_end:#x})"
                    )
                placed.append((base, size, name))
                resolved[name] = {
                    'base': base,
                    'size': size,
                    'align': alloc['align'],
                    'region': region_name,
                }
            else:
                auto_allocs.append((name, alloc))

        # Sort fixed placements by base address
        placed.sort()

        # Check for overlaps among fixed allocations
        for i in range(1, len(placed)):
            prev_base, prev_size, prev_name = placed[i - 1]
            cur_base, _, cur_name = placed[i]
            if prev_base + prev_size > cur_base:
                die(
                    f"allocations '{prev_name}' and '{cur_name}' overlap "
                    f"in region '{region_name}'"
                )

        # Build free gaps list
        gaps = []  # (start, end)
        cursor = region_base
        for base, size, _ in placed:
            if cursor < base:
                gaps.append((cursor, base))
            cursor = base + size
        if cursor < region_end:
            gaps.append((cursor, region_end))

        # Sort auto allocations largest-first for better packing
        auto_allocs.sort(key=lambda x: x[1]['size'], reverse=True)

        # First-fit auto allocations
        for name, alloc in auto_allocs:
            size = alloc['size']
            align = alloc['align']
            fitted = False

            for gi, (gap_start, gap_end) in enumerate(gaps):
                # Align up within this gap
                aligned_start = gap_start
                if align > 1:
                    aligned_start = (gap_start + align - 1) & ~(align - 1)

                if aligned_start + size <= gap_end:
                    resolved[name] = {
                        'base': aligned_start,
                        'size': size,
                        'align': align,
                        'region': region_name,
                    }
                    # Split the gap
                    new_gaps = []
                    if gap_start < aligned_start:
                        new_gaps.append((gap_start, aligned_start))
                    if aligned_start + size < gap_end:
                        new_gaps.append((aligned_start + size, gap_end))
                    gaps[gi:gi + 1] = new_gaps
                    fitted = True
                    break

            if not fitted:
                die(
                    f"allocation '{name}' (size {size:#x}, align {align:#x}) "
                    f"does not fit in region '{region_name}'"
                )

    return resolved


def parse_allocation_acl(doc, allocations):
    """Extract uses-allocation from tasks.

    Returns a dict mapping task_name -> [{name, permissions}, ...].
    """
    acl = {}
    for task_node in find_tasks(doc):
        tname = task_name(task_node)
        task_allocs = []
        for c in find_children(task_node, 'uses-allocation'):
            alloc_name = node_arg(c)
            if alloc_name not in allocations:
                die(
                    f"task '{tname}' uses-allocation '{alloc_name}' "
                    f"which is not defined"
                )
            perms = [child.name for child in c.nodes]
            task_allocs.append({
                'name': alloc_name,
                'permissions': perms,
            })
        if task_allocs:
            acl[tname] = task_allocs

    # Enforce: each allocation may be used by at most one task
    alloc_owners = {}
    for tname, task_allocs in acl.items():
        for entry in task_allocs:
            aname = entry['name']
            if aname in alloc_owners:
                die(
                    f"allocation '{aname}' is used by both "
                    f"'{alloc_owners[aname]}' and '{tname}' — "
                    f"each allocation may only be used by one task"
                )
            alloc_owners[aname] = tname

    return acl


# ---------------------------------------------------------------------------
# Pin configuration
# ---------------------------------------------------------------------------


def parse_chip_pin_capabilities(chip_doc):
    """Parse pin capability nodes from the chip KDL document.

    Returns a dict mapping pin_name -> list of (kind, instance, signals) tuples.
    instance is a string or None (None means "any instance").
    signals is a set of signal names, or None for no-signal peripherals (e.g. gpio).
    """
    capabilities = {}
    for pin_node in find_children(chip_doc, 'pin'):
        pin_name = node_name_arg(pin_node)
        if pin_name is None:
            die("pin node missing name")
        caps = []
        for sup in find_children(pin_node, 'supports'):
            if not sup.args:
                die(f"pin '{pin_name}': supports node missing peripheral kind")
            kind = sup.args[0].value
            instance = None
            if len(sup.args) > 1:
                raw = sup.args[1].value
                if isinstance(raw, str):
                    instance = raw
                elif isinstance(raw, float) and raw == int(raw):
                    instance = str(int(raw))
                else:
                    instance = str(raw)
            signals = {child.name for child in sup.nodes} if sup.nodes else None
            caps.append((kind, instance, signals))
        capabilities[pin_name] = caps
    return capabilities


def _pin_supports(caps, kind, instance, signal):
    """Check if a pin's capabilities support a given (kind, instance, signal).

    Returns True if:
    - There's an exact (kind, instance) match with signal in signals, or
    - There's a wildcard (kind, None) match with signal in signals, or
    - There's a (kind, instance) match with signals=None (no-signal peripheral)
    """
    for cap_kind, cap_instance, cap_signals in caps:
        if cap_kind != kind:
            continue
        # Instance must match exactly, or cap must be wildcard (None)
        if cap_instance is not None and cap_instance != str(instance):
            continue
        if cap_instance is None and instance is not None:
            # Wildcard cap — matches any instance
            pass
        # Signal check
        if signal is None:
            return True
        if cap_signals is not None and ('any' in cap_signals or signal in cap_signals):
            return True
    return False


def _build_known_peripherals(chip_capabilities):
    """Derive the set of known peripheral kinds from chip pin capabilities."""
    kinds = set()
    for caps in chip_capabilities.values():
        for kind, _instance, _signals in caps:
            kinds.add(kind)
    return kinds


def parse_pin_config(doc, chip_capabilities):
    """Parse board-level pin assignments and validate against chip capabilities.

    Reads peripheral blocks (usart, i2c, etc.) from the board node.
    Each block has the form:  kind [instance] { signal "PA{n}"; ... }

    Returns a list of assignment dicts:
        [{ pin, kind, instance, signal }, ...]
    """
    board_node = next(find_children(doc, 'board'), None)
    if board_node is None:
        return []

    known_peripherals = _build_known_peripherals(chip_capabilities)
    assignments = []
    pins_used = {}  # pin_name -> description string (for conflict errors)

    for node in board_node.nodes:
        if node.name not in known_peripherals:
            continue

        kind = node.name
        instance = None
        if node.args:
            raw = node.args[0].value
            if isinstance(raw, str):
                instance = raw
            elif isinstance(raw, float) and raw == int(raw):
                instance = str(int(raw))
            else:
                instance = str(raw)

        for child in node.nodes:
            signal = child.name

            if not child.args:
                die(
                    f"board pin config: {kind}"
                    f"{' ' + instance if instance else ''}"
                    f" {signal} missing pin assignment"
                )

            pin_name = child.args[0].value
            if not isinstance(pin_name, str) or not pin_name.startswith('PA'):
                die(
                    f"board pin config: {kind}"
                    f"{' ' + instance if instance else ''}"
                    f" {signal}: expected pin name like 'PA18',"
                    f" got '{pin_name}'"
                )

            # Check pin exists in chip capabilities
            if pin_name not in chip_capabilities:
                die(
                    f"board pin config: {pin_name} does not exist on this chip"
                )

            # Check pin supports this peripheral+signal
            caps = chip_capabilities[pin_name]
            if not _pin_supports(caps, kind, instance, signal):
                die(
                    f"board pin config: {pin_name} does not support"
                    f" {kind}"
                    f"{' ' + instance if instance else ''}"
                    f" {signal}"
                )

            # Conflict detection
            desc = (
                f"{kind}"
                f"{' ' + instance if instance else ''}"
                f" {signal}"
            )
            if pin_name in pins_used:
                die(
                    f"board pin config: {pin_name} assigned to both"
                    f" '{pins_used[pin_name]}' and '{desc}'"
                )
            pins_used[pin_name] = desc

            assignments.append({
                'pin': pin_name,
                'kind': kind,
                'instance': instance,
                'signal': signal,
            })

    return assignments


def resolve_chip_path(chip_path_str, input_path):
    """Resolve a chip path (possibly proj:-prefixed) to an absolute path."""
    if chip_path_str.startswith('proj:'):
        proj_rel = chip_path_str[len('proj:'):]
        return str(Path(input_path).parent / proj_rel)
    return chip_path_str


def get_chip_path(doc, input_path):
    """Get the resolved chip file path from the board node, or None."""
    board_node = next(find_children(doc, 'board'), None)
    if board_node is None:
        return None
    chip_node = find_child(board_node, 'chip')
    if chip_node is None:
        return None
    return resolve_chip_path(node_arg(chip_node), input_path)


def parse_chip_memory_regions(chip_path):
    """Parse memory regions from the chip KDL file.

    Returns a dict mapping region_name -> {base, size}.
    """
    with open(chip_path) as f:
        chip_doc = kdl.parse(f.read(), PARSE_CONFIG)

    regions = {}
    for mem_node in find_children(chip_doc, 'memory'):
        for region_node in find_children(mem_node, 'region'):
            name = node_name_arg(region_node)
            base_node = find_child(region_node, 'base')
            size_node = find_child(region_node, 'size')
            if base_node is None or size_node is None:
                continue
            base = int(float(node_arg(base_node)))
            size = int(float(node_arg(size_node)))
            regions[name] = {'base': base, 'size': size}
    return regions


def write_modified_chip(chip_path, resolved_allocations, output_path,
                        ftab_info=None):
    """Read the chip KDL, apply modifications, write modified copy.

    Modifications:
    - Add allocation peripherals (if resolved_allocations is non-empty).
    - Rewrite vectors/flash memory regions to match ftab boot_target
      (if ftab_info is provided).

    Returns the path to the modified chip file.
    """
    with open(chip_path) as f:
        chip_doc = kdl.parse(f.read(), PARSE_CONFIG)

    for name, alloc in sorted(resolved_allocations.items()):
        periph_name = f'alloc_{name}'
        periph_node = kdl.Node(
            name='peripheral',
            args=[kdl.String(value=periph_name, tag=None)],
            nodes=[
                kdl.Node(
                    name='base',
                    args=[kdl.Decimal(mantissa=alloc['base'], exponent=0, tag=None)],
                ),
                kdl.Node(
                    name='size',
                    args=[kdl.Decimal(mantissa=alloc['size'], exponent=0, tag=None)],
                ),
            ],
        )
        chip_doc.nodes.append(periph_node)

    # Rewrite vectors/flash regions to match ftab boot_target
    if ftab_info is not None:
        boot_addr = ftab_info['boot_target_addr']
        boot_size = ftab_info['boot_target_size']

        for mem_node in find_children(chip_doc, 'memory'):
            for region_node in find_children(mem_node, 'region'):
                rname = node_name_arg(region_node)
                if rname == 'vectors':
                    vec_size_node = find_child(region_node, 'size')
                    vec_size = int(float(node_arg(vec_size_node)))
                    base_node = find_child(region_node, 'base')
                    base_node.args = [
                        kdl.Decimal(mantissa=boot_addr, exponent=0, tag=None),
                    ]
                elif rname == 'flash':
                    # vectors region must be parsed first — it precedes flash
                    # in both chip KDL files.
                    vec_size_node = 0
                    for r in find_children(mem_node, 'region'):
                        if node_name_arg(r) == 'vectors':
                            vec_size_node = find_child(r, 'size')
                            break
                    vec_size = int(float(node_arg(vec_size_node)))
                    flash_base = boot_addr + vec_size
                    flash_size = boot_size - vec_size

                    base_node = find_child(region_node, 'base')
                    base_node.args = [
                        kdl.Decimal(mantissa=flash_base, exponent=0, tag=None),
                    ]
                    size_node = find_child(region_node, 'size')
                    size_node.args = [
                        kdl.Decimal(mantissa=flash_size, exponent=0, tag=None),
                    ]

    # Write modified chip alongside the output
    content = chip_doc.print(PRINT_CONFIG)
    content_hash = hashlib.sha256(content.encode()).hexdigest()[:12]
    out_dir = os.path.dirname(output_path) or '.'
    mod_chip_path = os.path.join(out_dir, f'chip.{content_hash}.kdl')
    os.makedirs(out_dir, exist_ok=True)
    with open(mod_chip_path, 'w') as f:
        f.write(content)

    return mod_chip_path


def transform_allocation_uses(doc):
    """Transform uses-allocation into uses-peripheral in task nodes."""
    for task_node in find_tasks(doc):
        new_nodes = []
        for c in task_node.nodes:
            if c.name == 'uses-allocation':
                alloc_name = node_arg(c)
                periph_name = f'alloc_{alloc_name}'
                new_nodes.append(
                    kdl.Node(
                        name='uses-peripheral',
                        args=[kdl.String(value=periph_name, tag=None)],
                    )
                )
            else:
                new_nodes.append(c)
        task_node.nodes = new_nodes


def update_board_chip_ref(doc, mod_chip_path, input_path):
    """Update the board's chip reference to point to the modified chip file.

    Converts the absolute path back to a proj:-relative path.
    """
    proj_dir = str(Path(input_path).parent)
    rel_path = os.path.relpath(mod_chip_path, proj_dir).replace('\\', '/')
    chip_ref = f'proj:{rel_path}'
    for board_node in find_children(doc, 'board'):
        chip_node = find_child(board_node, 'chip')
        if chip_node is not None:
            chip_node.args = [kdl.String(value=chip_ref, tag=None)]


# ---------------------------------------------------------------------------
# Task directive checks and transforms
# ---------------------------------------------------------------------------


def check_no_raw_uses_task(doc):
    """Error if any task block contains bare `uses-task`."""
    for task_node in find_tasks(doc):
        tname = task_name(task_node)
        if any(c.name == 'uses-task' for c in task_node.nodes):
            die(
                f"task '{tname}' uses bare 'uses-task'; "
                f"use 'uses-sysmodule X' or 'unsafe-uses-task X' instead"
            )


def check_dependency_cycles(doc):
    """Build a dependency graph from task directives and error on cycles.

    peer-sysmodule is intentionally excluded — it's the cycle-breaker.
    """
    all_names = collect_task_names(doc)
    graph = {name: set() for name in all_names}

    for task_node in find_tasks(doc):
        tname = task_name(task_node)

        for c in find_children(task_node, 'uses-sysmodule'):
            dep = f'sysmodule_{node_arg(c)}'
            if dep in graph:
                graph[tname].add(dep)

        for c in find_children(task_node, 'unsafe-uses-task'):
            dep = node_arg(c)
            if dep in graph:
                graph[tname].add(dep)

        has_partition = any(True for _ in find_children(task_node, 'uses-partition'))
        if has_partition and 'sysmodule_storage' in graph:
            graph[tname].add('sysmodule_storage')

        has_notif = (
            any(True for _ in find_children(task_node, 'uses-notification'))
            or any(True for _ in find_children(task_node, 'pushes-notification'))
        )
        if has_notif and 'sysmodule_reactor' in graph:
            graph[tname].add('sysmodule_reactor')

    # DFS cycle detection
    WHITE, GRAY, BLACK = 0, 1, 2
    color = {name: WHITE for name in all_names}
    path = []

    def dfs(node):
        color[node] = GRAY
        path.append(node)
        for dep in sorted(graph[node]):
            if color[dep] == GRAY:
                idx = path.index(dep)
                return path[idx:] + [dep]
            if color[dep] == WHITE:
                result = dfs(dep)
                if result:
                    return result
        path.pop()
        color[node] = BLACK
        return None

    for name in all_names:
        if color[name] == WHITE:
            cycle = dfs(name)
            if cycle:
                chain = ' -> '.join(cycle)
                print(
                    f"error: dependency cycle detected: {chain}",
                    file=sys.stderr,
                )
                print(
                    f"hint: use 'peer-sysmodule' instead of 'uses-sysmodule' "
                    f"to break the cycle",
                    file=sys.stderr,
                )
                sys.exit(1)


def transform_task_directives(doc):
    """Transform preprocessor directives into uses-task lines for hubake.

    Returns (peers_dict, uses_dict, priorities_dict).
    """
    all_names = collect_task_names(doc)
    all_peers = {}
    all_uses = {}
    all_priorities = {}

    for task_node in find_tasks(doc):
        tname = task_name(task_node)
        uses_tasks = set()
        real_uses = set()
        task_priorities = {}

        # uses-sysmodule X -> sysmodule_X
        for c in find_children(task_node, 'uses-sysmodule'):
            dep = f'sysmodule_{node_arg(c)}'
            uses_tasks.add(dep)
            real_uses.add(dep)

            # Extract with-priority from children if present
            prio_node = find_child(c, 'with-priority')
            if prio_node is not None:
                prio_val = node_arg(prio_node)
                if prio_val is None:
                    die(f"with-priority in task '{tname}' uses-sysmodule "
                        f"'{node_arg(c)}' requires an integer value")
                task_priorities[dep] = int(prio_val)
                # Strip children so they don't leak into hubake output
                c.nodes = []

        if task_priorities:
            all_priorities[tname] = task_priorities

        # peer-sysmodule X -> sysmodule_X (recorded as peer)
        peer_targets = [node_arg(c) for c in find_children(task_node, 'peer-sysmodule')]
        for p in peer_targets:
            uses_tasks.add(f'sysmodule_{p}')
        if peer_targets:
            all_peers[tname] = [f'sysmodule_{p}' for p in peer_targets]

        # unsafe-uses-task X or *
        for c in find_children(task_node, 'unsafe-uses-task'):
            val = node_arg(c)
            if val == '*':
                for tn in all_names:
                    if tn != tname:
                        uses_tasks.add(tn)
                        real_uses.add(tn)
            else:
                uses_tasks.add(val)
                real_uses.add(val)

        # uses-partition -> implicit sysmodule_storage
        if any(True for _ in find_children(task_node, 'uses-partition')):
            uses_tasks.add('sysmodule_storage')
            real_uses.add('sysmodule_storage')

        # uses-notification / pushes-notification -> implicit sysmodule_reactor
        if any(True for _ in find_children(task_node, 'uses-notification')):
            uses_tasks.add('sysmodule_reactor')
            real_uses.add('sysmodule_reactor')
        if any(True for _ in find_children(task_node, 'pushes-notification')):
            uses_tasks.add('sysmodule_reactor')
            real_uses.add('sysmodule_reactor')

        if real_uses:
            all_uses[tname] = sorted(real_uses)

        # Strip preprocessor directives, inject uses-task nodes
        strip_names = {
            'uses-sysmodule', 'peer-sysmodule', 'unsafe-uses-task',
        }
        task_node.nodes = [
            c for c in task_node.nodes if c.name not in strip_names
        ]
        for t in sorted(uses_tasks):
            task_node.nodes.append(
                kdl.Node(name='uses-task', args=[kdl.String(value=t, tag=None)])
            )

    return all_peers, all_uses, all_priorities


def inject_storage_uses_task(doc, partition_acl):
    """Add uses-task entries to sysmodule_storage for tasks that use partitions."""
    for task_node in find_tasks(doc):
        if task_name(task_node) != 'sysmodule_storage':
            continue
        existing = set()
        for c in find_children(task_node, 'uses-task'):
            existing.add(node_arg(c))
        for tname in sorted(partition_acl.keys()):
            if tname != 'sysmodule_storage' and tname not in existing:
                task_node.nodes.append(
                    kdl.Node(name='uses-task', args=[kdl.String(value=tname, tag=None)])
                )
        break


def ensure_supervisor_first(doc):
    """Move the task with a `supervisor` child to be the first task node.

    The `supervisor` node is stripped since hubake doesn't understand it.
    """
    tasks = list(find_tasks(doc))
    if not tasks:
        return

    supervisor_task = None
    for task_node in tasks:
        if find_child(task_node, 'supervisor') is not None:
            supervisor_task = task_node
            break

    if supervisor_task is None:
        return

    # Strip the supervisor directive
    supervisor_task.nodes = [
        c for c in supervisor_task.nodes if c.name != 'supervisor'
    ]

    # Move to be the first task
    first_task = tasks[0]
    if supervisor_task is first_task:
        return

    doc_nodes = doc.nodes
    sup_idx = doc_nodes.index(supervisor_task)
    first_idx = doc_nodes.index(first_task)
    doc_nodes.remove(supervisor_task)
    doc_nodes.insert(first_idx, supervisor_task)


# ---------------------------------------------------------------------------
# Output assembly
# ---------------------------------------------------------------------------


def build_hubake_doc(doc, strip_board_children=frozenset()):
    """Build a new document with only hubake-relevant nodes."""
    strip_top_level = {'define', 'block-device', 'filesystem', 'notifications', 'allocation'}
    strip_task_children = {'uses-partition', 'uses-notification', 'pushes-notification', 'uses-allocation'}
    strip_board = {'memory', 'block-device'} | set(strip_board_children)

    new_nodes = []
    for node in doc.nodes:
        if node.name in strip_top_level:
            continue
        if node.name == 'task':
            node.nodes = [
                c for c in node.nodes if c.name not in strip_task_children
            ]
        elif node.name == 'board':
            node.nodes = [
                c for c in node.nodes
                if c.name not in strip_board
            ]
        new_nodes.append(node)

    doc.nodes = new_nodes


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main():
    if len(sys.argv) < 3:
        print(f"usage: {sys.argv[0]} <input.kdl> <output.kdl> [--features A,B,C] [--board NAME]", file=sys.stderr)
        sys.exit(1)

    input_path = sys.argv[1]
    output_path = sys.argv[2]

    # Parse flags
    enabled_features = set()
    selected_board = None
    rest = sys.argv[3:]
    i = 0
    while i < len(rest):
        if rest[i] == '--features' and i + 1 < len(rest):
            for f in rest[i + 1].split(','):
                f = f.strip()
                if f:
                    enabled_features.add(f)
            i += 2
        elif rest[i] == '--board' and i + 1 < len(rest):
            selected_board = rest[i + 1]
            i += 2
        else:
            die(f"unexpected argument: {rest[i]}")

    with open(input_path) as f:
        text = f.read()

    doc = kdl.parse(text, PARSE_CONFIG)

    # Select board (strip non-matching board blocks)
    board_nodes = list(find_children(doc, 'board'))
    if selected_board is not None:
        matching = [b for b in board_nodes if node_name_arg(b) == selected_board]
        if not matching:
            available = [node_name_arg(b) for b in board_nodes]
            die(f"board '{selected_board}' not found. Available: {available}")
        doc.nodes = [n for n in doc.nodes
                     if n.name != 'board' or node_name_arg(n) == selected_board]
    elif len(board_nodes) > 1:
        available = [node_name_arg(b) for b in board_nodes]
        die(f"multiple boards defined, use --board to select one: {available}")

    # Apply feature gates (must be first — strips nodes before validation)
    apply_feature_gates(doc, enabled_features)

    # Error on bare uses-task (must use uses-sysmodule or unsafe-uses-task)
    check_no_raw_uses_task(doc)

    # Check for dependency cycles (peer-sysmodule excluded — it's the breaker)
    check_dependency_cycles(doc)

    # Parse and validate block-device / filesystem sections
    devices = parse_block_devices(doc)
    filesystems = parse_filesystems(doc)

    partition_acl = {}
    if devices:
        partition_acl = parse_uses_partition(doc, devices)
    if filesystems:
        validate_filesystems(filesystems, devices)

    # Parse and validate notification groups
    notification_groups = parse_notifications(doc)
    notif_pushers, notif_subscribers = {}, {}
    if notification_groups:
        notif_pushers, notif_subscribers = parse_notification_acl(
            doc, notification_groups,
        )

    # Parse and solve allocations
    memory_regions = parse_board_memory(doc)
    allocations = parse_allocations(doc, memory_regions)
    resolved_allocations = {}
    allocation_acl = {}
    if allocations:
        resolved_allocations = solve_allocations(allocations, memory_regions)
        allocation_acl = parse_allocation_acl(doc, allocations)
        transform_allocation_uses(doc)

    # Transform task directives (uses-sysmodule, peer-sysmodule, etc.)
    peers, uses, priorities = transform_task_directives(doc)

    # Inject uses-task entries into sysmodule_storage for ACL enforcement
    inject_storage_uses_task(doc, partition_acl)

    # Resolve defines and size annotations
    defines = parse_defines(doc)
    for node in doc.nodes:
        resolve_defines(node, defines)
        resolve_size_annotations(node)

    # Reorder supervisor task to be first
    ensure_supervisor_first(doc)

    # Collect ftab info from all devices (at most one)
    ftab_info = None
    for dev in devices.values():
        if dev.get('ftab') is not None:
            ftab_info = dev['ftab']
            break

    # Parse chip memory regions and inject allocation peripherals
    chip_path = get_chip_path(doc, input_path)
    chip_regions = {}
    pin_assignments = []
    known_pin_peripherals = set()
    if chip_path:
        chip_regions = parse_chip_memory_regions(chip_path)

        # Parse and validate pin configuration against chip capabilities
        with open(chip_path) as f:
            chip_doc = kdl.parse(f.read(), PARSE_CONFIG)
        chip_capabilities = parse_chip_pin_capabilities(chip_doc)
        if chip_capabilities:
            known_pin_peripherals = _build_known_peripherals(chip_capabilities)
            pin_assignments = parse_pin_config(doc, chip_capabilities)

        if resolved_allocations or ftab_info:
            mod_chip_path = write_modified_chip(
                chip_path, resolved_allocations, output_path,
                ftab_info=ftab_info,
            )
            update_board_chip_ref(doc, mod_chip_path, input_path)
            # Re-parse so chip-regions.json reflects the modified layout
            chip_regions = parse_chip_memory_regions(mod_chip_path)

    # Generate ftab binary if an ftab partition was declared
    if ftab_info is not None:
        out_dir = os.path.dirname(output_path) or '.'
        ftab_bin = os.path.join(out_dir, 'ftab.bin')
        scripts_dir = os.path.join(os.path.dirname(__file__))
        gen_ftab = os.path.join(scripts_dir, 'gen_ftab.py')
        import subprocess
        result = subprocess.run(
            [
                sys.executable, gen_ftab,
                '--ftab-addr', str(ftab_info['ftab_addr']),
                '--target-addr', str(ftab_info['boot_target_addr']),
                '--target-size', str(ftab_info['boot_target_size']),
                '--output', ftab_bin,
            ],
            capture_output=True, text=True,
        )
        if result.returncode != 0:
            die(f"gen_ftab.py failed:\n{result.stderr}")
        # Print gen_ftab output for visibility
        if result.stdout.strip():
            print(result.stdout.strip(), file=sys.stderr)

    # Strip non-hubake sections
    build_hubake_doc(doc, strip_board_children=known_pin_peripherals)

    # Write output
    output = doc.print(PRINT_CONFIG)
    os.makedirs(os.path.dirname(output_path) or '.', exist_ok=True)
    with open(output_path, 'w') as f:
        f.write(output)

    # Write partition table JSON alongside the output
    if devices or filesystems:
        json_path = output_path.rsplit('.', 1)[0] + '.partitions.json'
        partition_data = {
            'devices': {
                name: dev['partitions'] for name, dev in devices.items()
            },
            'device_sizes': {
                name: dev['size'] for name, dev in devices.items()
                if dev['size'] is not None
            },
            'device_block_sizes': {
                name: dev['block_size'] for name, dev in devices.items()
            },
            'device_mappings': {
                name: dev['mapping'] for name, dev in devices.items()
                if dev['mapping'] is not None
            },
            'filesystems': {
                name: maps for name, maps in filesystems.items()
            },
            'partition_acl': partition_acl,
        }
        if ftab_info is not None:
            partition_data['ftab'] = ftab_info
        with open(json_path, 'w') as f:
            json.dump(partition_data, f, indent=2)

    # Write peers JSON alongside the output
    if peers:
        json_path = output_path.rsplit('.', 1)[0] + '.peers.json'
        with open(json_path, 'w') as f:
            json.dump(peers, f, indent=2)

    # Write uses JSON alongside the output (real deps, excluding peers)
    if uses:
        json_path = output_path.rsplit('.', 1)[0] + '.uses.json'
        with open(json_path, 'w') as f:
            json.dump(uses, f, indent=2)

    # Write priorities JSON alongside the output
    if priorities:
        json_path = output_path.rsplit('.', 1)[0] + '.priorities.json'
        with open(json_path, 'w') as f:
            json.dump(priorities, f, indent=2)

    # Write chip memory regions JSON alongside the output
    if chip_regions:
        json_path = output_path.rsplit('.', 1)[0] + '.chip-regions.json'
        with open(json_path, 'w') as f:
            json.dump(chip_regions, f, indent=2)

    # Write allocations JSON alongside the output
    if resolved_allocations:
        json_path = output_path.rsplit('.', 1)[0] + '.allocations.json'
        alloc_data = {
            'regions': memory_regions,
            'allocations': {
                name: {
                    'base': info['base'],
                    'size': info['size'],
                    'align': info['align'],
                    'region': info['region'],
                    'peripheral': f'alloc_{name}',
                }
                for name, info in resolved_allocations.items()
            },
            'acl': allocation_acl,
        }
        with open(json_path, 'w') as f:
            json.dump(alloc_data, f, indent=2)

    # Write pin configuration JSON alongside the output
    if pin_assignments:
        json_path = output_path.rsplit('.', 1)[0] + '.pins.json'
        with open(json_path, 'w') as f:
            json.dump({'assignments': pin_assignments}, f, indent=2)

    # Write notification groups JSON alongside the output
    if notification_groups:
        json_path = output_path.rsplit('.', 1)[0] + '.notifications.json'
        notif_data = {
            'groups': notification_groups,
            'pushers': notif_pushers,
            'subscribers': notif_subscribers,
        }
        with open(json_path, 'w') as f:
            json.dump(notif_data, f, indent=2)


if __name__ == '__main__':
    main()
