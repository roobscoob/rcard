import zipfile, os, shutil
if os.path.exists("elf"):
    shutil.rmtree("elf")
z = zipfile.ZipFile("extracted.tfw")
for n in z.namelist():
    if n.startswith("elf/") or n in ("config.json", "build-metadata.json"):
        z.extract(n)
        print("extracted", n)
