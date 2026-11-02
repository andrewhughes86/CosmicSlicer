"""Flattens an OrcaSlicer system preset: its `inherits` chain and `include`s applied, as Orca's
GUI does when it loads the library. Orca's command line does not, so tools/parity.sh hands it
presets flattened here.

Usage: python3 tools/parity_flatten.py <profiles dir> <vendor/kind/name.json> <out.json>
"""
import json, os, sys

META = {"inherits", "include", "instantiation", "setting_id", "base_id", "filament_id", "description"}


def index(profiles, vendor, kind):
    """name -> file path, for one vendor's kind folder (and Orca's common filament library)."""
    out = {}
    dirs = [os.path.join(profiles, vendor, kind)]
    if kind == "filament" and vendor != "OrcaFilamentLibrary":
        dirs.append(os.path.join(profiles, "OrcaFilamentLibrary", kind))
    for d in reversed(dirs):
        for root, _, files in os.walk(d):
            for f in files:
                if f.endswith(".json"):
                    p = os.path.join(root, f)
                    try:
                        name = json.load(open(p, encoding="utf-8")).get("name")
                    except (OSError, ValueError):
                        continue
                    if name:
                        out[name] = p
    return out


def resolve(names, path, depth=0):
    if depth > 20:
        raise ValueError("inherits loop at " + path)
    raw = json.load(open(path, encoding="utf-8"))
    values = {}
    parent = raw.get("inherits")
    if parent:
        values.update(resolve(names, names[parent], depth + 1))
    includes = raw.get("include") or []
    for inc in includes if isinstance(includes, list) else [includes]:
        values.update(resolve(names, names[inc], depth + 1))
    values.update({k: v for k, v in raw.items() if k not in META})
    return values


if __name__ == "__main__":
    profiles, rel, out = sys.argv[1], sys.argv[2], sys.argv[3]
    vendor, kind = rel.split("/")[:2]
    v = resolve(index(profiles, vendor, kind), os.path.join(profiles, rel))
    v["from"] = "system"
    v["instantiation"] = "true"
    json.dump(v, open(out, "w", encoding="utf-8"), indent=1)
