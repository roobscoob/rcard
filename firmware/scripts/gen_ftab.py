#!/usr/bin/env python3
"""
gen_ftab.py - Generate SiFli SF32LB52 flash partition table (ftab) binary.

Produces a 20KB .bin suitable for flashing to 0x10000000 on the N4 variant.
"""

import argparse
import struct
import sys

# ── Constants from mem_map.h / dfu.h ─────────────────────────────────────────

FLASH_TABLE_SIZE    = 20 * 1024          # 0x5000 - reserved partition size
FTAB_BASE_DEFAULT   = 0x10000000         # MPI1 base (N4 SiP NOR)
TARGET_ADDR_DEFAULT = 0x10010000         # FLASH_BOOT_PATCH_START_ADDR

SEC_CONFIG_MAGIC    = 0x53454346

# Offsets within sec_configuration
SECFG_FTAB_OFFSET   = 4                  # ftab[0] starts at byte 4
SECFG_IMG_OFFSET    = 0x1000             # imgs[] starts at 4096
SECFG_RUNIMG_OFFSET = 0x2C00             # running_imgs[] starts here

# struct flash_table: 4 x uint32 = 16 bytes
FLASH_TABLE_ENTRY_SIZE = 16

# struct image_header_enc: 512 bytes
IMAGE_HEADER_ENC_SIZE = 512

# DFU constants
DFU_FLASH_IMG_LCPU  = 2                  # first image slot index
DFU_FLASH_IMG_BL    = 3                  # bootloader/second-stage flash slot
DFU_FLASH_PARTITION = 16                 # total ftab entries
CORE_BL             = 1                  # index into running_imgs[]

FLASH_UNINIT_32     = 0xFFFFFFFF


# ── Helpers ───────────────────────────────────────────────────────────────────

def flash_table_entry(base=0, size=0, xip_base=0, flags=0):
    return struct.pack("<IIII", base, size, xip_base, flags)

def image_header_enc(length=FLASH_UNINIT_32, blksize=0, flags=0):
    """
    struct image_header_enc {
        uint32_t length;      // 4
        uint16_t blksize;     // 2
        uint16_t flags;       // 2
        uint8_t  key[32];     // 32
        uint8_t  sig[256];    // 256
        uint8_t  ver[20];     // 20
        uint8_t  reserved[196]; // 196
    };  // total 512 bytes
    """
    hdr  = struct.pack("<IHH", length, blksize, flags)
    hdr += b"\xff" * 32   # key
    hdr += b"\xff" * 256  # sig
    hdr += b"\xff" * 20   # ver
    hdr += b"\xff" * 196  # reserved
    assert len(hdr) == IMAGE_HEADER_ENC_SIZE
    return hdr


# ── Main ──────────────────────────────────────────────────────────────────────

def build_ftab(ftab_base, target_addr, target_size):
    buf = bytearray(b"\xff" * FLASH_TABLE_SIZE)

    # ── magic (offset 0x0000) ─────────────────────────────────────────────────
    struct.pack_into("<I", buf, 0, SEC_CONFIG_MAGIC)

    # ── ftab[] (offset 0x0004, 16 entries × 16 bytes) ────────────────────────
    #
    # ftab[0] = self-reference (the flash table partition itself)
    # ftab[3] = second-stage bootloader (DFU_FLASH_IMG_BL)
    #
    # All other entries remain 0xFF.

    def write_ftab(idx, base, size, xip_base, flags=0):
        off = SECFG_FTAB_OFFSET + idx * FLASH_TABLE_ENTRY_SIZE
        struct.pack_into("<IIII", buf, off, base, size, xip_base, flags)

    write_ftab(0, ftab_base, FLASH_TABLE_SIZE, 0, 0)
    write_ftab(3, target_addr, target_size, target_addr, 0)

    # ── imgs[] (offset 0x1000, 14 entries × 512 bytes) ───────────────────────
    #
    # imgs index = DFU_FLASH_IMG_IDX(flash_id) = flash_id - DFU_FLASH_IMG_LCPU
    # DFU_FLASH_IMG_BL = 3  →  imgs index = 3 - 2 = 1
    #
    # flags MUST be 0 — if 0xFF the ROM sees DFU_FLAG_ENC set and crashes.

    def write_img(imgs_idx, length, flags=0):
        off = SECFG_IMG_OFFSET + imgs_idx * IMAGE_HEADER_ENC_SIZE
        entry = image_header_enc(length=length, blksize=0, flags=flags)
        buf[off:off + IMAGE_HEADER_ENC_SIZE] = entry

    imgs_bl_idx = DFU_FLASH_IMG_BL - DFU_FLASH_IMG_LCPU  # = 1
    write_img(imgs_bl_idx, target_size, flags=0)

    # ── running_imgs[] (offset 0x2C00, 4 × uint32) ───────────────────────────
    #
    # running_imgs[CORE_BL=1] must be the flash address of imgs[1]:
    #   ftab_base + 0x1000 + imgs_bl_idx * 512

    running_img_ptr = ftab_base + SECFG_IMG_OFFSET + imgs_bl_idx * IMAGE_HEADER_ENC_SIZE

    struct.pack_into("<I", buf, SECFG_RUNIMG_OFFSET + CORE_BL * 4, running_img_ptr)

    return bytes(buf)


def main():
    parser = argparse.ArgumentParser(
        description="Generate SiFli SF32LB52 ftab binary (20KB)."
    )
    parser.add_argument(
        "--target-addr",
        type=lambda x: int(x, 0),
        default=TARGET_ADDR_DEFAULT,
        help=f"Flash address of second-stage binary (default: {TARGET_ADDR_DEFAULT:#010x})",
    )
    parser.add_argument(
        "--target-size",
        type=lambda x: int(x, 0),
        required=True,
        help="Size of second-stage binary in bytes (e.g. 0x8000 or 32768)",
    )
    parser.add_argument(
        "--ftab-addr",
        type=lambda x: int(x, 0),
        default=FTAB_BASE_DEFAULT,
        help=f"Flash base address of ftab partition (default: {FTAB_BASE_DEFAULT:#010x})",
    )
    parser.add_argument(
        "--output",
        required=True,
        help="Output file path (e.g. ftab.bin)",
    )
    args = parser.parse_args()

    if args.target_size <= 0:
        print("error: --target-size must be > 0", file=sys.stderr)
        sys.exit(1)

    data = build_ftab(args.ftab_addr, args.target_addr, args.target_size)
    assert len(data) == FLASH_TABLE_SIZE

    with open(args.output, "wb") as f:
        f.write(data)

    imgs_bl_idx = DFU_FLASH_IMG_BL - DFU_FLASH_IMG_LCPU
    running_img_ptr = args.ftab_addr + SECFG_IMG_OFFSET + imgs_bl_idx * IMAGE_HEADER_ENC_SIZE

    print(f"ftab written to {args.output!r} ({FLASH_TABLE_SIZE} bytes)")
    print(f"  magic             = {SEC_CONFIG_MAGIC:#010x}")
    print(f"  ftab[0].base      = {args.ftab_addr:#010x}  (self)")
    print(f"  ftab[3].base      = {args.target_addr:#010x}  (second-stage)")
    print(f"  ftab[3].xip_base  = {args.target_addr:#010x}")
    print(f"  ftab[3].size      = {args.target_size:#010x}")
    print(f"  imgs[1].length    = {args.target_size:#010x}")
    print(f"  imgs[1].flags     = 0x00000000  (plaintext, no encryption)")
    print(f"  running_imgs[1]   = {running_img_ptr:#010x}")
    print()
    print("Flash layout:")
    print(f"  {args.ftab_addr:#010x}  ftab.bin     (this file, 20KB)")
    print(f"  {args.target_addr:#010x}  second-stage ({args.target_size} bytes)")


if __name__ == "__main__":
    main()