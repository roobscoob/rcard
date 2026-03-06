#!/usr/bin/env python3
"""
KDL preprocessor: resolves named constants defined in `define` blocks.

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
"""

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

    defines, pattern = parse_defines(text)
    output = resolve(text, defines, pattern) if defines else text

    with open(sys.argv[2], 'w') as f:
        f.write(output)


if __name__ == '__main__':
    main()
