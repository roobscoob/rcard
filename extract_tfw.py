import struct, sys, os

src = r"host/target/debug/app.exe"
data = open(src, "rb").read()
print("size:", len(data))

# Find every EOCD signature, parse central directory, and pick the one whose
# central directory contains "log-metadata.json" — that's the tfw.
eocd_sig = b"PK\x05\x06"
hits = []
i = 0
while True:
    j = data.find(eocd_sig, i)
    if j < 0:
        break
    hits.append(j)
    i = j + 1
print("EOCD candidates:", len(hits))

picked = None
for j in hits:
    try:
        sig, disk, cdisk, ndisk, total, csize, coff, clen = struct.unpack_from("<IHHHHIIH", data, j)
        if sig != 0x06054b50:
            continue
        # The central directory record is `csize` bytes ending right before the EOCD,
        # but its absolute file offset is unknown until we know the zip start.
        # Strategy: search backward for PK\x01\x02 within a generous window.
        cd_sig = b"PK\x01\x02"
        cd_pos = data.rfind(cd_sig, max(0, j - csize - 0x100), j)
        if cd_pos < 0:
            continue
        zip_start = cd_pos - coff
        if zip_start < 0:
            continue
        # Read all filenames in the central directory.
        names = []
        p = cd_pos
        for _ in range(total):
            if data[p:p+4] != cd_sig:
                break
            name_len = struct.unpack_from("<H", data, p+28)[0]
            extra_len = struct.unpack_from("<H", data, p+30)[0]
            comm_len = struct.unpack_from("<H", data, p+32)[0]
            name = data[p+46:p+46+name_len].decode(errors="replace")
            names.append(name)
            p += 46 + name_len + extra_len + comm_len
        if "log-metadata.json" in names:
            zip_end = j + 22 + clen
            picked = (zip_start, zip_end, names)
            break
    except Exception as e:
        continue

if not picked:
    print("no tfw zip found")
    sys.exit(1)

zip_start, zip_end, names = picked
print("tfw zip at", hex(zip_start), "..", hex(zip_end), "size", zip_end - zip_start)
print("entries:", names)
out = data[zip_start:zip_end]
open("extracted.tfw", "wb").write(out)
print("wrote extracted.tfw,", len(out), "bytes")
