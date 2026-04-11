import struct, sys, io
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")

data = open("elf/kernel", "rb").read()
# rodata vaddr 0x20056bac, file off 0x3b2c
# HUBRIS_REGION_DESCS at vaddr 0x20057438, size 0x378
o = 0x3b2c + (0x20057438 - 0x20056bac)
size = 0x378
print(f"region descs at file offset {hex(o)}, size {size}")

# RegionDesc layout (kern/src/descs.rs):
#   arch_data: RegionDescExt  (architecture specific)
#   base: u32
#   size: u32
#   attributes: RegionAttributes (u32)
#
# armv8m RegionDescExt has rlar, rbar, mair (3 * u32 = 12 bytes)
# armv7m  has rasr, rbar (2 * u32 = 8 bytes)
# Try both sizes.

for stride in (24, 20, 16):
    if size % stride == 0:
        print(f"\n=== assuming stride {stride} ===")
        n = size // stride
        for i in range(n):
            rec = data[o + i * stride : o + i * stride + stride]
            if stride == 24:
                a1, a2, a3, base, sz, attr = struct.unpack("<IIIIII", rec)
                print(f"  [{i:2}] base=0x{base:08x} size=0x{sz:06x} attr=0x{attr:08x} arch=(0x{a1:08x},0x{a2:08x},0x{a3:08x})")
            elif stride == 20:
                a1, a2, base, sz, attr = struct.unpack("<IIIII", rec)
                print(f"  [{i:2}] base=0x{base:08x} size=0x{sz:06x} attr=0x{attr:08x} arch=(0x{a1:08x},0x{a2:08x})")
            else:
                a1, a2, base, sz = struct.unpack("<IIII", rec)
                print(f"  [{i:2}] base=0x{base:08x} size=0x{sz:06x} arch=(0x{a1:08x},0x{a2:08x})")
        break
