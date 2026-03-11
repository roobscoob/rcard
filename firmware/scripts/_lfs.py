"""
LittleFS helper for sdmmc.nu.

Usage:
    _lfs.py ls     <img> <offset> <size> <path>
    _lfs.py read   <img> <offset> <size> <path>
    _lfs.py format <img> <offset> <size> [<source_dir>]

Operates on a slice of a disk image file at the given byte offset/size.
"""

import os
import sys

import littlefs


BLOCK_SIZE = 512


def make_fs(block_count):
    """Create a LittleFS instance with our standard config.

    Parameters must match the embedded littlefs config in state.rs.
    """
    return littlefs.LittleFS(
        block_size=BLOCK_SIZE,
        block_count=block_count,
        read_size=BLOCK_SIZE,
        prog_size=BLOCK_SIZE,
        lookahead_size=16,
        name_max=31,
        block_cycles=500,
    )


def read_partition(img_path, offset, size):
    """Read a partition's raw bytes from the disk image."""
    with open(img_path, "rb") as f:
        f.seek(offset)
        return f.read(size)


def write_partition(img_path, offset, data):
    """Write data back to a partition region in the disk image."""
    with open(img_path, "r+b") as f:
        f.seek(offset)
        f.write(data)


def mount_partition(img_path, offset, size):
    """Mount a littlefs partition from the disk image."""
    block_count = size // BLOCK_SIZE
    data = read_partition(img_path, offset, size)
    fs = make_fs(block_count)
    fs.context.buffer = bytearray(data)
    fs.mount()
    return fs


def cmd_ls(img_path, offset, size, path):
    fs = mount_partition(img_path, offset, size)
    try:
        entries = fs.listdir(path)
    except Exception as e:
        print(f"error: {e}", file=sys.stderr)
        sys.exit(1)

    for name in sorted(entries):
        full = os.path.join(path, name).replace("\\", "/")
        try:
            stat = fs.stat(full)
            kind = "d" if stat.type == 2 else "-"
            print(f"{kind} {stat.size:>10}  {name}")
        except Exception:
            print(f"? {0:>10}  {name}")


def cmd_read(img_path, offset, size, path):
    fs = mount_partition(img_path, offset, size)
    try:
        with fs.open(path, "rb") as f:
            data = f.read()
    except Exception as e:
        print(f"error: {e}", file=sys.stderr)
        sys.exit(1)

    sys.stdout.buffer.write(data)


def cmd_format(img_path, offset, size, source_dir=None):
    block_count = size // BLOCK_SIZE
    fs = make_fs(block_count)
    fs.format()
    fs.mount()

    if source_dir:
        populate(fs, source_dir, "/")

    # Write the formatted filesystem back to the image
    data = bytes(fs.context.buffer)
    # Pad to full partition size if needed
    if len(data) < size:
        data += b"\xff" * (size - len(data))
    write_partition(img_path, offset, data[:size])


def populate(fs, host_dir, lfs_path):
    """Recursively copy a host directory into the littlefs."""
    for entry in sorted(os.listdir(host_dir)):
        host_path = os.path.join(host_dir, entry)
        target = lfs_path.rstrip("/") + "/" + entry

        if os.path.isdir(host_path):
            try:
                fs.mkdir(target)
            except Exception:
                pass  # already exists
            populate(fs, host_path, target)
        else:
            with open(host_path, "rb") as src:
                data = src.read()
            with fs.open(target, "wb") as dst:
                dst.write(data)


def main():
    if len(sys.argv) < 5:
        print(__doc__, file=sys.stderr)
        sys.exit(1)

    cmd = sys.argv[1]
    img_path = sys.argv[2]
    offset = int(sys.argv[3])
    size = int(sys.argv[4])

    if cmd == "ls":
        if len(sys.argv) < 6:
            print("usage: _lfs.py ls <img> <offset> <size> <path>", file=sys.stderr)
            sys.exit(1)
        cmd_ls(img_path, offset, size, sys.argv[5])

    elif cmd == "read":
        if len(sys.argv) < 6:
            print("usage: _lfs.py read <img> <offset> <size> <path>", file=sys.stderr)
            sys.exit(1)
        cmd_read(img_path, offset, size, sys.argv[5])

    elif cmd == "format":
        source_dir = sys.argv[5] if len(sys.argv) > 5 else None
        cmd_format(img_path, offset, size, source_dir)

    else:
        print(f"unknown command: {cmd}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
