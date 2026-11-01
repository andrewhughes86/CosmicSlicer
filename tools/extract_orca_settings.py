"""Extracts OrcaSlicer's option definitions and settings-tab layout into JSON."""
import json, re, sys

src = sys.argv[1]
out = sys.argv[2]
pc = open(f"{src}/libslic3r/PrintConfig.cpp", encoding="utf-8").read()
tab = open(f"{src}/slic3r/GUI/Tab.cpp", encoding="utf-8").read()

def strip_comments(s):
    res, i, n = [], 0, len(s)
    while i < n:
        c = s[i]
        if c == '"':
            j = i + 1
            while j < n and s[j] != '"':
                j += 2 if s[j] == '\\' else 1
            res.append(s[i:j+1]); i = j + 1
        elif s.startswith('//', i):
            j = s.find('\n', i); i = n if j < 0 else j
        elif s.startswith('/*', i):
            j = s.find('*/', i); i = n if j < 0 else j + 2
        else:
            res.append(c); i += 1
    return ''.join(res)

def statements(s):
    out, cur, i, n = [], [], 0, len(s)
    depth = 0
    while i < n:
        c = s[i]
        if c == '"':
            j = i + 1
            while j < n and s[j] != '"':
                j += 2 if s[j] == '\\' else 1
            cur.append(s[i:j+1]); i = j + 1; continue
        if c in '([': depth += 1
        if c in ')]': depth -= 1
        if c == ';' :
            out.append(''.join(cur).strip()); cur = []
        elif c in '{}' and depth <= 0:
            depth = 0
            if ''.join(cur).strip(): out.append(''.join(cur).strip())
            cur = []
        else:
            cur.append(c)
        i += 1
    return out

def unescape(lit):
    lit = re.sub(r'\\u([0-9a-fA-F]{4})', lambda m: chr(int(m.group(1), 16)), lit)
    lit = re.sub(r'\\(.)', lambda m: {'n': '\n', 't': '\t'}.get(m.group(1), m.group(1)), lit)
    # The UI font has the degree sign but not the one-glyph Celsius sign.
    return lit.replace('\u2103', '°C')

STR = re.compile(r'"((?:[^"\\]|\\.)*)"')
def strings(stmt):
    return ''.join(unescape(m) for m in STR.findall(stmt))

pc = strip_comments(pc)
# enum constant -> key
enum_const = {}
for m in re.finditer(r's_keys_map_(\w+)\s*=?\s*\{(.*?)\};', pc, re.S):
    for k, v in re.findall(r'\{\s*"([^"]*)"\s*,\s*(?:int\s*\(\s*)?([\w:]+)\s*\)?\s*\}', m.group(2)):
        enum_const[v.split('::')[-1]] = k

opts = {}
named = {}
cur = None
for st in statements(pc):
    m = re.match(r'(?:auto\s+(\w+)\s*=\s*)?def\s*=\s*this->add(?:_nullable)?\(\s*"(\w+)"\s*,\s*(\w+)\s*\)', st)
    if m:
        cur = {"type": m.group(3)[2:], "enum": []}
        opts[m.group(2)] = cur
        if m.group(1): named[m.group(1)] = cur
        continue
    m = re.match(r'def->enum_(values|labels)\s*=\s*(\w+)->enum_\1', st)
    if m and cur is not None and m.group(2) in named:
        src_enum = named[m.group(2)]['enum']
        if m.group(1) == 'values': cur['enum'] = [[v, None] for v, _ in src_enum]
        else: cur['enum'] = [[v, l] for (v, _), (_, l) in zip(cur['enum'], src_enum)]
        continue
    if cur is None or not st.startswith('def->'):
        if re.match(r'def\s*=', st): cur = None
        continue
    m = re.match(r'def->(\w+)\s*(=|\.push_back|->)?(.*)', st, re.S)
    if not m: continue
    field, rest = m.group(1), m.group(3)
    if field in ('label', 'full_label', 'tooltip', 'sidetext', 'category'):
        cur[field] = strings(rest)
    elif field in ('min', 'max'):
        v = rest.strip().lstrip('=').strip()
        try: cur[field] = float(v)
        except ValueError: pass
    elif field == 'mode':
        m_ = rest.strip().lstrip('=').strip().replace('com', '').lower()
        if m_ in ('simple', 'advanced', 'expert', 'develop'): cur['mode'] = m_
    elif field == 'gui_type':
        cur['gui_type'] = rest.split('::')[-1].strip()
    elif field == 'enum_values':
        cur['enum'].append([strings(rest), None])
    elif field == 'enum_labels':
        lab = strings(rest)
        for e in cur['enum']:
            if e[1] is None: e[1] = lab; break
    elif field == 'height':
        try: cur['height'] = int(rest.strip().lstrip('=').strip())
        except ValueError: pass
    elif field == 'multiline':
        cur['multiline'] = 'true' in rest
    elif field == 'set_default_value':
        dm = re.search(r'new\s+ConfigOption(\w+?)(?:<\w+>)?\s*[\({](.*)[\)}]\s*\)\s*$', rest, re.S)
        if not dm:
            cur['default'] = ''
            continue
        kind, arg = dm.group(1), dm.group(2).strip()
        if kind.startswith('Point'):
            pts = re.findall(r'Vec2d\(\s*([-\d.]+)\s*,\s*([-\d.]+)\s*\)', arg)
            cur['default'] = ','.join(f'{x}x{y}' for x, y in pts)
        elif '"' in arg:
            cur['default'] = STR.findall(arg) and unescape(STR.findall(arg)[0]) or ''
        elif kind.startswith('Enum'):
            cur['default'] = enum_const.get(arg.split('::')[-1].strip(), arg)
        elif kind.startswith('Bool'):
            cur['default'] = 'true' in arg.split(',')[0] or arg.split(',')[0].strip() == '1'
        elif kind.startswith('FloatOrPercent') or kind.startswith('FloatsOrPercents'):
            a = re.findall(r'[-\d.]+|true|false', arg)
            cur['default'] = (a[0] + ('%' if 'true' in a[1:2] else '')) if a else ''
        elif kind.startswith('Percent'):
            a = re.findall(r'[-\d.]+', arg)
            cur['default'] = (a[0] + '%') if a else ''
        else:
            a = re.findall(r'-?\d+(?:\.\d+)?(?:e-?\d+)?', arg)
            cur['default'] = a[0] if a else arg[:40]
for o in opts.values():
    o['enum'] = [[v, l if l is not None else v] for v, l in o['enum']]
    if not o['enum']: del o['enum']

def layout(func_names):
    pages = []
    for fn in func_names:
        m = re.search(r'^void ' + re.escape(fn) + r'\(.*?\)\s*\n\{(.*?)^\}', tab, re.S | re.M)
        if not m: print('missing', fn, file=sys.stderr); continue
        body = strip_comments(m.group(1))
        page = group = None
        pending = None
        line = None
        tok = re.compile(r'add_options_page\(\s*L\("([^"]+)"\)\s*,\s*"([^"]*)"'
                         r'|new_optgroup\(\s*L\("([^"]+)"\)\s*(?:,\s*L?"([^"]*)")?'
                         r'|append_single_option_line\(\s*"(\w+)"'
                         r'|get_option\(\s*"(\w+)"'
                         r'|append_single_option_line\(\s*option'
                         r'|(?:Line\s+)?line\s*=\s*\{\s*L\("([^"]+)"\)'
                         r'|create_line_with_widget\(\s*optgroup(?:\.get\(\))?\s*,\s*"(\w+)"'
                         r'|append_line\(')
        def add(k):
            if group is None: return
            if any(k == e.get('key') or k in e.get('keys', []) for e in group['options']): return
            group['options'].append({"key": k})
        for t in tok.finditer(body):
            if t.group(1):
                page = {"name": t.group(1), "icon": t.group(2), "groups": []}
                if not any(p['name'] == page['name'] for p in pages): pages.append(page)
                else: page = next(p for p in pages if p['name'] == page['name'])
                line = None
            elif t.group(3):
                group = {"name": t.group(3), "icon": t.group(4) or "", "options": []}
                if page is not None: page['groups'].append(group)
                line = None
            elif t.group(5):
                add(t.group(5)); line = None
            elif t.group(6):
                pending = t.group(6)
                if line is not None:
                    if pending not in line['keys']: line['keys'].append(pending)
                    pending = None
            elif t.group(7):
                line = {"line": t.group(7), "keys": []}
                if group is not None: group['options'].append(line)
            elif t.group(8):
                add(t.group(8)); group['options'][-1]['widget'] = True
            elif t.group(0).startswith('append_single_option_line') and pending:
                add(pending); pending = None
            elif t.group(0).startswith('append_line'):
                line = None
    for p in pages:
        for g in p['groups']:
            g['options'] = [e for e in g['options'] if e.get('key') or e.get('keys')]
        p['groups'] = [g for g in p['groups'] if g['options']]
    return [p for p in pages if p['groups']]

tabs = {
    "process": layout(['TabPrint::build']),
    "filament": layout(['TabFilament::build']),
    "printer": layout(['TabPrinter::build_fff', 'TabPrinter::build_unregular_pages']),
}
used = {k for t in tabs.values() for p in t for g in p['groups'] for e in g['options'] for k in ([e['key']] if 'key' in e else e['keys'])}
missing = sorted(k for k in used if k not in opts)
print('options', len(opts), 'used', len(used), 'missing', missing, file=sys.stderr)
for t, ps in tabs.items():
    print(t, [(p['name'], len(p['groups'])) for p in ps], file=sys.stderr)
# Defaults OrcaSlicer gives by named constants.
CONSTANTS = {'INITIAL_LAYER_HEIGHT': '0.2', 'INITIAL_RAFT_LAYERS': '0', 'SurfaceFillOrder::Default': 'default'}
for v in opts.values():
    if isinstance(v.get('default'), str) and v['default'] in CONSTANTS:
        v['default'] = CONSTANTS[v['default']]
keep = dict(opts)
for v in keep.values(): v.pop('category', None)
json.dump({"options": keep, "tabs": tabs}, open(out, 'w'), indent=1, ensure_ascii=False)
