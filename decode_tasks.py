import struct, sys, io
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")

data = open("elf/kernel", "rb").read()
# rodata vaddr 0x20056bac, file off 0x3b2c
# HUBRIS_TASK_DESCS at vaddr 0x20057304, size 0x134
o = 0x3b2c + (0x20057304 - 0x20056bac)
size = 0x134
print(f"task descs at file offset {hex(o)}, size {size}")

# TaskDesc layout (descs.rs):
#   regions: [&'static RegionDesc; 8]   = 8 * 4 bytes = 32
#   entry_point: u32 = 4
#   initial_stack: u32 = 4
#   priority: u8
#   index: u16
#   flags: u8
# Total: 32 + 4 + 4 + 1+ 2 + 1 (+ padding for 4-byte align) = 44 bytes
# But layout depends on field order/padding. Let me try 48 (round to 16).

stride = size // 7
print(f"7 tasks → stride {stride}")

# Region descs are 24 bytes; HUBRIS_REGION_DESCS is at 0x20057438
region_base_va = 0x20057438

for i in range(7):
    rec = data[o + i * stride : o + i * stride + stride]
    # 8 region pointers
    regs = struct.unpack_from("<8I", rec, 0)
    region_indices = [(p - region_base_va) // 24 if p else None for p in regs]
    entry_point, initial_stack = struct.unpack_from("<II", rec, 32)
    rest = rec[40:].hex()
    print(f"task {i}: regions={region_indices} entry=0x{entry_point:08x} sp=0x{initial_stack:08x} rest={rest}")
