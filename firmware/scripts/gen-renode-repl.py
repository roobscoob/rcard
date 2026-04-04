# /// script
# dependencies = ["kdl-py"]
# ///
"""
Generate a Renode .repl platform description from app and chip KDL files.

Resolves the chip KDL path from the board block in app.kdl, then reads
memory regions, peripherals, and renode model annotations to produce a
complete .repl platform description.

Usage:
    python gen-renode-repl.py <app.kdl> <output.repl>
"""

import os
import sys
from pathlib import Path

import kdl

PARSE_CONFIG = kdl.ParseConfig(
    nativeUntaggedValues=False,
    nativeTaggedValues=False,
)


# ---------------------------------------------------------------------------
# KDL helpers (same conventions as kdl-preprocess.py)
# ---------------------------------------------------------------------------

def node_arg(node, index=0):
    val = node.args[index] if index < len(node.args) else None
    if val is None:
        return None
    return val.value if hasattr(val, 'value') else val


def find_children(node, name):
    for child in node.nodes:
        if child.name == name:
            yield child


def find_child(node, name):
    return next(find_children(node, name), None)


def parse_int(val):
    """Parse a KDL numeric value that may be float-encoded hex."""
    return int(float(val))


def fmt_hex(n):
    return f"0x{n:08X}"


def resolve_chip_path(chip_ref, app_path):
    """Resolve a chip path (possibly proj:-prefixed) relative to app.kdl."""
    if chip_ref.startswith('proj:'):
        return str(Path(app_path).parent / chip_ref[len('proj:'):])
    return chip_ref


# ---------------------------------------------------------------------------
# Chip KDL parsing
# ---------------------------------------------------------------------------

def parse_chip(chip_path):
    with open(chip_path) as f:
        doc = kdl.parse(f.read(), PARSE_CONFIG)

    # Target triple
    triple_node = find_child(doc, 'target-triple')
    target_triple = node_arg(triple_node) if triple_node else None

    # Memory regions
    regions = []
    mem_node = find_child(doc, 'memory')
    if mem_node:
        for region in find_children(mem_node, 'region'):
            name = node_arg(region)
            base = parse_int(node_arg(find_child(region, 'base')))
            size = parse_int(node_arg(find_child(region, 'size')))
            regions.append({'name': name, 'base': base, 'size': size})

    # Peripherals
    peripherals = []
    for periph in find_children(doc, 'peripheral'):
        name = node_arg(periph)
        base = parse_int(node_arg(find_child(periph, 'base')))
        size = parse_int(node_arg(find_child(periph, 'size')))

        irq_node = find_child(periph, 'irq')
        irq = parse_int(node_arg(irq_node, 1)) if irq_node else None

        renode_node = find_child(periph, 'renode')
        model = None
        model_props = {}
        if renode_node:
            model_node = find_child(renode_node, 'model')
            if model_node:
                model = node_arg(model_node)
                for prop in model_node.nodes:
                    model_props[prop.name] = parse_int(node_arg(prop))

        peripherals.append({
            'name': name,
            'base': base,
            'size': size,
            'irq': irq,
            'model': model,
            'model_props': model_props,
        })

    return target_triple, regions, peripherals


# ---------------------------------------------------------------------------
# App KDL parsing
# ---------------------------------------------------------------------------

def parse_app(app_path, board_name=None):
    """Parse app KDL, returning (chip_path, board_memory_regions)."""
    with open(app_path) as f:
        doc = kdl.parse(f.read(), PARSE_CONFIG)

    if board_name:
        board_node = next(
            (n for n in find_children(doc, 'board') if node_arg(n) == board_name),
            None,
        )
    else:
        board_node = find_child(doc, 'board')
    if board_node is None:
        print(f"error: no board block found in app.kdl"
              + (f" matching '{board_name}'" if board_name else ""),
              file=sys.stderr)
        sys.exit(1)

    # Resolve chip path
    chip_node = find_child(board_node, 'chip')
    if chip_node is None:
        print("error: board block has no chip reference", file=sys.stderr)
        sys.exit(1)
    chip_path = resolve_chip_path(node_arg(chip_node), app_path)

    # Board memory regions
    regions = []
    for mem in find_children(board_node, 'memory'):
        name = node_arg(mem)
        base = parse_int(node_arg(find_child(mem, 'base')))
        size = parse_int(node_arg(find_child(mem, 'size')))
        regions.append({'name': name, 'base': base, 'size': size})

    return chip_path, regions


# ---------------------------------------------------------------------------
# REPL generation
# ---------------------------------------------------------------------------

CPU_TYPE_MAP = {
    'thumbv8m.main-none-eabihf': 'cortex-m33',
    'thumbv7em-none-eabihf': 'cortex-m4f',
    'thumbv7m-none-eabi': 'cortex-m3',
    'thumbv6m-none-eabi': 'cortex-m0plus',
}


def coalesce_regions(regions):
    """Merge contiguous memory regions into single entries."""
    if not regions:
        return regions
    # Sort by base address
    sorted_regions = sorted(regions, key=lambda r: r['base'])
    merged = [dict(sorted_regions[0])]
    for region in sorted_regions[1:]:
        prev = merged[-1]
        if region['base'] == prev['base'] + prev['size']:
            # Contiguous — extend the previous region
            prev['name'] = prev['name'] + '_' + region['name']
            prev['size'] += region['size']
        else:
            merged.append(dict(region))
    return merged


def generate_repl(target_triple, chip_regions, board_regions, peripherals):
    lines = []

    # CPU + NVIC
    cpu_type = CPU_TYPE_MAP.get(target_triple, 'cortex-m33')
    lines.append(f'cpu: CPU.CortexM @ sysbus')
    lines.append(f'    cpuType: "{cpu_type}"')
    lines.append(f'    nvic: nvic')
    lines.append(f'')
    lines.append(f'nvic: IRQControllers.NVIC @ sysbus 0xE000E000')
    lines.append(f'    -> cpu@0')
    lines.append(f'')

    # Memory regions (merge contiguous regions)
    lines.append(f'// Memory')
    lines.append(f'')
    for region in coalesce_regions(chip_regions):
        lines.append(f'{region["name"]}: Memory.MappedMemory @ sysbus {fmt_hex(region["base"])}')
        lines.append(f'    size: {fmt_hex(region["size"])}')
        lines.append(f'')
    for region in coalesce_regions(board_regions):
        lines.append(f'{region["name"]}: Memory.MappedMemory @ sysbus {fmt_hex(region["base"])}')
        lines.append(f'    size: {fmt_hex(region["size"])}')
        lines.append(f'')

    # Modeled peripherals
    modeled = [p for p in peripherals if p['model']]
    silenced = [p for p in peripherals if not p['model']]

    if modeled:
        lines.append(f'// Peripherals')
        lines.append(f'')
        for p in modeled:
            lines.append(f'{p["name"]}: {p["model"]} @ sysbus {fmt_hex(p["base"])}')
            for key, val in p['model_props'].items():
                lines.append(f'    {key}: {val}')
            if p['irq'] is not None:
                lines.append(f'    IRQ -> nvic@{p["irq"]}')
            lines.append(f'')

    # Silence ranges
    if silenced:
        lines.append(f'sysbus:')
        lines.append(f'    init:')
        for p in silenced:
            end = p['base'] + p['size'] - 1
            lines.append(f'        SilenceRange <{fmt_hex(p["base"])}, {fmt_hex(end)}>')
        lines.append(f'')

    return '\n'.join(lines)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('app_kdl', help='Path to app.kdl')
    parser.add_argument('output', help='Output .repl path')
    parser.add_argument('--board', help='Board name to select')
    args = parser.parse_args()

    app_path, output_path = args.app_kdl, args.output

    chip_path, board_regions = parse_app(app_path, args.board)
    target_triple, chip_regions, peripherals = parse_chip(chip_path)

    repl = generate_repl(target_triple, chip_regions, board_regions, peripherals)

    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    with open(output_path, 'w') as f:
        f.write(repl)


if __name__ == '__main__':
    main()
