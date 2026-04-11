import json, sys, io
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")
c = json.load(open("config.json"))
print("top-level keys:", list(c.keys()))
print()
print("=== entries (first one) ===")
entries = c["entries"]
print("type:", type(entries).__name__, "len:", len(entries) if hasattr(entries, "__len__") else "?")
if isinstance(entries, list):
    print(json.dumps(entries[0], indent=2)[:3000] if entries else "(empty)")
elif isinstance(entries, dict):
    for k in list(entries.keys())[:3]:
        print(f"--- {k} ---")
        print(json.dumps(entries[k], indent=2)[:1500])


