#!/usr/bin/env python3
"""Line-level comparison of OrcaSlicer's G-code and ours (ROADMAP session 19).

Usage: tools/parity_diff.py <case dir> [--layer N] [--feature NAME] [--show K]
       tools/parity_diff.py --all <work dir>

Reads <case>/orca.gcode and <case>/ours.gcode (from tools/parity.sh) and reports how far they
are from identical, level by level, each level ignoring only what the one before it blames:

  text       the raw lines are the same
  moves      same moves in the same order, once number format, modal X/Y/F and comments go
  feeds      same moves, ignoring the F values
  extrusion  same moves and feeds, ignoring the E values
  order      each feature prints the same segments, in another order or direction
  geometry   the segments themselves differ (where and how much is listed by feature)

With --layer it prints that layer's two move lists side by side from the first difference.
"""
import argparse
import collections
import math
import os
import re
import sys

WORD = re.compile(r"([A-Z])(-?\d*\.?\d+)")
# Orca's feature names as our G-code writes them are the same words; "Custom" etc. pass through.
LAYER = re.compile(r"^(; CHANGE_LAYER|;LAYER_CHANGE)")


class Move:
    __slots__ = ("cmd", "x", "y", "z", "e", "f", "feature", "extra")

    def __init__(self, cmd, x, y, z, e, f, feature, extra=""):
        self.cmd, self.x, self.y, self.z, self.e, self.f, self.feature, self.extra = cmd, x, y, z, e, f, feature, extra

    def key(self, feeds=True, extrusion=True):
        out = [self.cmd, round(self.x, 3), round(self.y, 3), round(self.z, 3), self.extra]
        if extrusion:
            out.append(round(self.e, 4))
        if feeds:
            out.append(round(self.f))
        return tuple(out)

    def __str__(self):
        return f"{self.cmd} X{self.x:.3f} Y{self.y:.3f} Z{self.z:.3f} E{self.e:.5f} F{self.f:.0f} {self.extra}[{self.feature}]"


def read(path):
    """Sections of the file: [(name, raw lines, moves)], 'head', then one per layer."""
    sections = []
    raw, moves, name = [], [], "head"
    x = y = z = 0.0
    f = 0.0
    feature = ""
    abs_xyz = True
    abs_e = False
    last_e = 0.0
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if LAYER.match(line):
                sections.append((name, raw, moves))
                raw, moves, name = [], [], f"layer {len(sections)}"
            raw.append(line)
            code = line.split(";", 1)[0].strip()
            comment = line.split(";", 1)[1].strip() if ";" in line else ""
            for tag in ("FEATURE: ", "TYPE:"):
                if comment.startswith(tag):
                    feature = comment[len(tag):].strip()
            if not code:
                continue
            word = code.split()[0]
            if word == "G90":
                abs_xyz = True
            elif word == "G91":
                abs_xyz = False
            elif word == "M82":
                abs_e = True
            elif word == "M83":
                abs_e = False
            elif word == "G92":
                m = re.search(r"E(-?\d*\.?\d+)", code)
                last_e = float(m.group(1)) if m else last_e
            if word in ("G0", "G1", "G2", "G3"):
                vals = dict((k, float(v)) for k, v in WORD.findall(code[len(word):]))
                if "F" in vals:
                    f = vals["F"]
                moved = any(k in vals for k in "XYZE")
                if abs_xyz:
                    x, y, z = vals.get("X", x), vals.get("Y", y), vals.get("Z", z)
                else:
                    x, y, z = x + vals.get("X", 0), y + vals.get("Y", 0), z + vals.get("Z", 0)
                e = vals.get("E", 0.0)
                if abs_e and "E" in vals:
                    e, last_e = e - last_e, e
                if moved:
                    extra = "" if word in ("G0", "G1") else f"I{vals.get('I', 0):.3f}J{vals.get('J', 0):.3f}"
                    moves.append(Move("G1" if word == "G0" else word, x, y, z, e, f, feature, extra))
            else:
                moves.append(Move("T", 0, 0, 0, 0, 0, feature, code))
    sections.append((name, raw, moves))
    return sections


def segments(moves):
    """Undirected extruded segments by feature, rounded to 0.01 mm: {feature: Counter}."""
    out = collections.defaultdict(collections.Counter)
    px = py = None
    for m in moves:
        if m.cmd == "T":
            continue
        if m.e > 0 and px is not None and (m.x != px or m.y != py):
            a = (round(px, 2), round(py, 2))
            b = (round(m.x, 2), round(m.y, 2))
            out[m.feature][(min(a, b), max(a, b))] += 1
        px, py = m.x, m.y
    return out


def length(seg):
    (a, b) = seg
    return math.dist(a, b)


def only_motion(moves):
    return [m for m in moves if m.cmd != "T"]


def classify(a, b):
    """The first level at which two sections differ."""
    if a[1] == b[1]:
        return "text"
    ma, mb = a[2], b[2]
    if [m.key() for m in ma] == [m.key() for m in mb]:
        return "moves"
    ma, mb = only_motion(ma), only_motion(mb)
    if [m.key() for m in ma] == [m.key() for m in mb]:
        return "commands"
    if [m.key(feeds=False) for m in ma] == [m.key(feeds=False) for m in mb]:
        return "feeds"
    if [m.key(feeds=False, extrusion=False) for m in ma] == [m.key(feeds=False, extrusion=False) for m in mb]:
        return "extrusion"
    sa, sb = segments(ma), segments(mb)
    if sa == sb:
        return "order"
    return "geometry"


def compare(case):
    a = read(os.path.join(case, "orca.gcode"))
    b = read(os.path.join(case, "ours.gcode"))
    levels = collections.Counter()
    by_feature = collections.defaultdict(lambda: [0.0, 0.0, 0.0])  # orca, ours, shared length
    raw_same = raw_total = 0
    per_layer = []
    for sa, sb in zip(a, b):
        level = classify(sa, sb)
        levels[level] += 1
        per_layer.append((sa[0], level))
        ca, cb = collections.Counter(sa[1]), collections.Counter(sb[1])
        raw_same += sum((ca & cb).values())
        raw_total += max(len(sa[1]), len(sb[1]))
        ga, gb = segments(sa[2]), segments(sb[2])
        for feat in set(ga) | set(gb):
            x, y = ga.get(feat, collections.Counter()), gb.get(feat, collections.Counter())
            row = by_feature[feat]
            row[0] += sum(length(s) * n for s, n in x.items())
            row[1] += sum(length(s) * n for s, n in y.items())
            row[2] += sum(length(s) * n for s, n in (x & y).items())
    return a, b, levels, by_feature, raw_same, raw_total, per_layer


def report(case, out=sys.stdout):
    a, b, levels, by_feature, same, total, per_layer = compare(case)
    name = os.path.basename(case.rstrip("/"))
    print(f"== {name}: {len(a) - 1} / {len(b) - 1} layers; "
          f"{same}/{total} lines identical ({100 * same / max(total, 1):.1f}%)", file=out)
    print("   sections by first difference: " + ", ".join(f"{k} {v}" for k, v in levels.most_common()), file=out)
    print("   mm of line in exactly the same place, by feature:", file=out)
    for feat, (x, y, both) in sorted(by_feature.items(), key=lambda kv: -kv[1][0]):
        share = 100 * both / max(x, y, 1e-9)
        print(f"     {feat:<24} orca {x:>10.1f}  ours {y:>10.1f}  same {share:5.1f}%", file=out)
    return per_layer


def show_layer(case, layer, feature, k):
    a = read(os.path.join(case, "orca.gcode"))
    b = read(os.path.join(case, "ours.gcode"))
    i = 0 if layer == 0 else layer
    sa, sb = a[i], b[i]
    ma = [m for m in sa[2] if not feature or m.feature == feature]
    mb = [m for m in sb[2] if not feature or m.feature == feature]
    first = next((n for n, (p, q) in enumerate(zip(ma, mb)) if p.key() != q.key()), min(len(ma), len(mb)))
    print(f"{sa[0]}: {len(ma)} / {len(mb)} moves, first difference at {first}  ({classify(sa, sb)})")
    for n in range(max(0, first - 2), min(max(len(ma), len(mb)), first + k)):
        p = str(ma[n]) if n < len(ma) else "-"
        q = str(mb[n]) if n < len(mb) else "-"
        print(f"{n:5} orca {p}\n      ours {q}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("case")
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--layer", type=int)
    ap.add_argument("--feature")
    ap.add_argument("--show", type=int, default=20)
    args = ap.parse_args()
    if args.layer is not None:
        show_layer(args.case, args.layer, args.feature, args.show)
    elif args.all:
        for d in sorted(os.listdir(args.case)):
            case = os.path.join(args.case, d)
            if os.path.exists(os.path.join(case, "ours.gcode")) and os.path.exists(os.path.join(case, "orca.gcode")):
                report(case)
    else:
        report(args.case)


if __name__ == "__main__":
    main()
