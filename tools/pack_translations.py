"""Packs OrcaSlicer's translations into assets/i18n/translations.bin.

Orca's catalogues (localization/i18n/<lang>/OrcaSlicer_<lang>.po) are kept only for the
English texts CosmicSlicer shows: every string in assets/settings/orca_settings.json (labels,
tooltips, units, enum labels, page and group names) and every string literal in the UI source
(crates/ui-egui/src), so the pack stays small. Fuzzy and untranslated entries are dropped.

Layout: zlib-compressed JSON {"languages": [[code, name], ...], "strings": {code: {english: text}}}.

Usage: python3 tools/pack_translations.py <dir with OrcaSlicer_<lang>.po or <lang>.po files> assets/i18n/translations.bin
"""
import glob, json, os, re, sys, zlib

src, out = sys.argv[1], sys.argv[2]
NAMES = {
    "ca": "Català", "cs": "Čeština", "de": "Deutsch", "en": "English", "es": "Español",
    "eu": "Euskara", "fr": "Français", "hu": "Magyar", "it": "Italiano", "ja": "日本語",
    "ko": "한국어", "lt": "Lietuvių", "nl": "Nederlands", "pl": "Polski",
    "pt_BR": "Português (Brasil)", "ru": "Русский", "sv": "Svenska", "th": "ไทย",
    "tr": "Türkçe", "uk": "Українська", "vi": "Tiếng Việt", "zh_CN": "简体中文", "zh_TW": "繁體中文",
}

# What CosmicSlicer shows.
wanted = set()
settings = json.load(open("assets/settings/orca_settings.json"))
for opt in settings["options"].values():
    for k in ("label", "full_label", "tooltip", "sidetext", "category"):
        if isinstance(opt.get(k), str):
            wanted.add(opt[k])
    for k in ("enum_labels",):
        for v in opt.get(k) or []:
            wanted.add(v)
def walk(v):
    if isinstance(v, dict):
        for k, x in v.items():
            if k in ("name", "label", "title") and isinstance(x, str):
                wanted.add(x)
            walk(x)
    elif isinstance(v, list):
        for x in v:
            walk(x)
walk(settings["tabs"])
lit = re.compile(r'"((?:[^"\\\n]|\\.)*)"')
for path in glob.glob("crates/ui-egui/src/**/*.rs", recursive=True):
    for m in lit.finditer(open(path, encoding="utf-8").read()):
        s = m.group(1).replace('\\"', '"').replace("\\n", "\n").replace("\\\\", "\\")
        if any(c.isalpha() for c in s):
            wanted.add(s)

def unquote(s):
    s = s.strip()
    if not (s.startswith('"') and s.endswith('"')):
        return ""
    s = s[1:-1]
    return (s.replace('\\n', '\n').replace('\\t', '\t').replace('\\"', '"').replace('\\\\', '\\'))

def parse(path):
    entries, cur, key, fuzzy = {}, {}, None, False
    def flush():
        nonlocal cur, fuzzy
        mid, mstr = cur.get("msgid"), cur.get("msgstr")
        if mid and mstr and not fuzzy and "msgctxt" not in cur:
            entries[mid] = mstr
        cur, fuzzy = {}, False
    for line in open(path, encoding="utf-8"):
        line = line.rstrip("\n")
        if not line.strip():
            flush(); key = None; continue
        if line.startswith("#,") and "fuzzy" in line:
            fuzzy = True; continue
        if line.startswith("#"):
            continue
        for k in ("msgctxt", "msgid_plural", "msgid", "msgstr[0]", "msgstr"):
            if line.startswith(k + " "):
                key = "msgstr" if k.startswith("msgstr") else k
                if k.startswith("msgstr[") and k != "msgstr[0]":
                    key = None
                    break
                cur[key] = cur.get(key, "") + unquote(line[len(k):])
                break
        else:
            if line.startswith('"') and key:
                cur[key] = cur.get(key, "") + unquote(line)
    flush()
    return entries

pack = {"languages": [["en", "English"]], "strings": {}}
for code in sorted(NAMES):
    if code == "en":
        continue
    cands = [os.path.join(src, f"OrcaSlicer_{code}.po"), os.path.join(src, f"{code}.po")]
    path = next((p for p in cands if os.path.exists(p)), None)
    if not path:
        continue
    entries = parse(path)
    kept = {k: v for k, v in entries.items() if k in wanted and v != k}
    pack["languages"].append([code, NAMES[code]])
    pack["strings"][code] = kept
    print(f"{code}: {len(kept)} of {len(entries)} kept")
os.makedirs(os.path.dirname(out), exist_ok=True)
data = zlib.compress(json.dumps(pack, ensure_ascii=False, sort_keys=True).encode("utf-8"), 9)
open(out, "wb").write(data)
print(f"{len(wanted)} English strings wanted; {len(data) // 1024} KiB written")
