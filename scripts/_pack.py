"""Pack a file into an image at a given offset, zero-padding to fill the partition."""
import sys

file, img, offset, size = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])

with open(file, "rb") as f:
    data = f.read()

pad = b"\x00" * (size - len(data))

with open(img, "r+b") as f:
    f.seek(offset)
    f.write(data + pad)

print(f"Wrote {len(data)} bytes + {len(pad)} zero-padding at offset {offset}")
