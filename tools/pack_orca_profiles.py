"""Packs OrcaSlicer's resources/profiles into assets/profiles/orca_profiles.bin.

Layout: b"CSPROF1\\n", u32 little-endian index length, the index (JSON), then one zlib blob per
vendor. The index lists vendors, their printer models and printers (what the setup wizard
needs) and each blob's offset and length; a blob holds the vendor's profiles:
{"machine_model"|"machine"|"process"|"filament": {name: {key: value}}}.

Values become CosmicSlicer's flat strings: arrays of strings are joined with "," (one-element
arrays keep their element as is, so G-code is never split). `compatible_printers`, `include`
and `upward_compatible_machine` stay lists.

Usage: python3 tools/pack_orca_profiles.py <OrcaSlicer>/resources/profiles assets/profiles/orca_profiles.bin
"""
import json, os, struct, sys, zlib

src, out = sys.argv[1], sys.argv[2]
LISTS = {"compatible_printers", "include", "upward_compatible_machine"}

def flat(v):
    if isinstance(v, list):
        items = [x if isinstance(x, str) else json.dumps(x) for x in v]
        return items[0] if len(items) == 1 else ",".join(items)
    if isinstance(v, bool):
        return "1" if v else "0"
    return v if isinstance(v, str) else json.dumps(v)

def load(path):
    with open(path, encoding="utf-8-sig") as f:
        return json.load(f)

index = {"vendors": []}
blobs = []
offset = 0
for entry in sorted(os.listdir(src)):
    if not entry.endswith(".json") or entry == "blacklist.json":
        continue
    vendor_dir = entry[:-5]
    vendor = load(os.path.join(src, entry))
    if not isinstance(vendor, dict) or not os.path.isdir(os.path.join(src, vendor_dir)):
        continue
    data = {"machine_model": {}, "machine": {}, "process": {}, "filament": {}}
    kinds = {"machine_model_list": "machine_model", "machine_list": "machine", "process_list": "process", "filament_list": "filament"}
    for list_key, kind in kinds.items():
        for item in vendor.get(list_key, []):
            path = os.path.join(src, vendor_dir, item["sub_path"])
            try:
                d = load(path)
            except Exception as e:
                print("skip", path, e, file=sys.stderr)
                continue
            if not isinstance(d, dict):
                continue
            name = d.get("name", item["name"])
            data[kind][name] = {k: (v if k in LISTS and isinstance(v, list) else flat(v)) for k, v in d.items()}
    models = []
    for name, m in data["machine_model"].items():
        models.append({
            "name": name,
            "family": m.get("family", ""),
            "nozzles": [n for n in m.get("nozzle_diameter", "").split(";") if n],
            "default_materials": [x for x in m.get("default_materials", "").split(";") if x],
        })
    printers = []
    for name, p in data["machine"].items():
        if p.get("instantiation", "true") == "true":
            printers.append({"name": name, "model": p.get("printer_model", ""), "variant": p.get("printer_variant", "")})
    blob = zlib.compress(json.dumps(data, separators=(",", ":"), ensure_ascii=False).encode(), 9)
    index["vendors"].append({
        "id": vendor_dir,
        "name": vendor.get("name", vendor_dir),
        "version": vendor.get("version", ""),
        "models": models,
        "printers": printers,
        "offset": offset,
        "length": len(blob),
    })
    blobs.append(blob)
    offset += len(blob)

idx = json.dumps(index, separators=(",", ":"), ensure_ascii=False).encode()
with open(out, "wb") as f:
    f.write(b"CSPROF1\n")
    f.write(struct.pack("<I", len(idx)))
    f.write(idx)
    for b in blobs:
        f.write(b)
print(len(index["vendors"]), "vendors,", sum(len(v["printers"]) for v in index["vendors"]), "printers,",
      os.path.getsize(out) // 1024, "KiB", file=sys.stderr)
