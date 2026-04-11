import struct, re, sys, io
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")

data = open(r"host/target/debug/app.exe", "rb").read()
print("size:", len(data))

eocd_hits = [m.start() for m in re.finditer(rb"PK\x05\x06", data)]
print("eocd hits:", [hex(x) for x in eocd_hits])
print()

for j in eocd_hits:
    sig, disk, cdisk, ndisk, total, csize, coff, clen = struct.unpack_from("<IHHHHIIH", data, j)
    print(f"EOCD @ {hex(j)}: total={total} csize={hex(csize)} coff={hex(coff)} clen={clen}")
    # The CD record(s) should be `csize` bytes, ending right before the EOCD.
    cd_end = j
    cd_start = cd_end - csize
    print(f"  → CD range {hex(cd_start)}..{hex(cd_end)}")
    if data[cd_start:cd_start+4] == b"PK\x01\x02":
        zip_start = cd_start - coff
        print(f"  → CD signature OK at {hex(cd_start)}, zip_start={hex(zip_start)}")
        # parse names
        p = cd_start
        names = []
        for _ in range(total):
            if data[p:p+4] != b"PK\x01\x02":
                break
            name_len = struct.unpack_from("<H", data, p+28)[0]
            extra_len = struct.unpack_from("<H", data, p+30)[0]
            comm_len = struct.unpack_from("<H", data, p+32)[0]
            name = data[p+46:p+46+name_len].decode(errors="replace")
            names.append(name)
            p += 46 + name_len + extra_len + comm_len
        print(f"  → {len(names)} names: {names[:5]}...")
        if "log-metadata.json" in names:
            print("  *** this is the tfw ***")
            zip_end = j + 22 + clen
            out = data[zip_start:zip_end]
            open("extracted.tfw", "wb").write(out)
            print(f"  wrote extracted.tfw, {len(out)} bytes")
    else:
        print(f"  → CD signature NOT at {hex(cd_start)}; bytes: {data[cd_start:cd_start+4].hex()}")
    print()
