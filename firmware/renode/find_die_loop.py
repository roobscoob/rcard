"""Find the post-epitaph infinite loop address in die_impl.

Disassembles the kernel ELF and locates the last `b .` (self-branch)
instruction within kern::fail::die_impl. This is the point where the
KERNEL_EPITAPH buffer has been fully written and the CPU spins forever.

Prints the decimal address to stdout for use by run.nu.
"""
import re
import subprocess
import sys

kernel_elf = sys.argv[1]

out = subprocess.check_output(
    ["rust-objdump", "-d", "--no-show-raw-insn", kernel_elf],
    text=True,
)

in_die_impl = False
last_self_branch = None

# objdump output format:
#   Function header:  "10004434 <_ZN4kern4fail8die_impl17h...E>:"
#   Instruction:      "10004434:      \tpush\t{r7, lr}"
#   Blank line or section header breaks functions

for line in out.splitlines():
    # Function header: "addr <symbol>:"
    header = re.match(r"^[0-9a-f]+ <(.+)>:\s*$", line)
    if header:
        if in_die_impl:
            break  # next function — we're done
        if "die_impl" in header.group(1):
            in_die_impl = True
        continue

    # Blank / section lines end a function block
    if in_die_impl and not line.strip():
        break

    if in_die_impl:
        # Instruction: "100044a4:  \tb\t0x100044a4 <...>"
        m = re.match(r"^([0-9a-f]+):\s+b\t0x([0-9a-f]+)\b", line)
        if m and m.group(1) == m.group(2):
            last_self_branch = m.group(1)

if last_self_branch is None:
    print("ERROR: could not find self-branch in die_impl", file=sys.stderr)
    sys.exit(1)

print(int(last_self_branch, 16))
