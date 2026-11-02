"""Writes the generated models of the parity reference set (tools/parity.sh) as binary STLs.

Usage: python3 tools/parity_models.py <out dir>

  cube20      20 mm calibration cube
  small       5 mm cube
  large       150 x 100 x 3 mm plate (mostly solid infill)
  cylinder    30 mm round, 20 tall (curved walls, seams)
  thinwalls   fins 0.3 to 1.6 mm thick on a base (thin walls, gap fill, wall transitions)
  overhangs   ramps leaning 20 to 70 degrees from vertical (overhang degrees and speeds)
  bridge      a 20 mm bridge between two pillars
  steps       a stepped pyramid (top and bottom surfaces at many heights)
"""
import math, os, struct, sys


def area(poly):
    return sum(x0 * y1 - x1 * y0 for (x0, y0), (x1, y1) in zip(poly, poly[1:] + poly[:1])) / 2


def ear_clip(poly):
    """Triangles of a simple counter-clockwise polygon, as index triples."""
    idx = list(range(len(poly)))
    tris = []

    def inside(p, a, b, c):
        def s(p, q, r):
            return (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
        return s(a, b, p) >= 0 and s(b, c, p) >= 0 and s(c, a, p) >= 0

    while len(idx) > 3:
        for k in range(len(idx)):
            i, j, l = idx[k - 1], idx[k], idx[(k + 1) % len(idx)]
            a, b, c = poly[i], poly[j], poly[l]
            if (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]) <= 0:
                continue
            if any(inside(poly[m], a, b, c) for m in idx if m not in (i, j, l)):
                continue
            tris.append((i, j, l))
            idx.pop(k)
            break
        else:
            raise ValueError("not a simple polygon")
    tris.append(tuple(idx))
    return tris


def prism(poly, z0, z1, plane="xy", depth0=0.0):
    """`poly` extruded: in the XY plane from z0 to z1, or in the XZ plane (poly's y is z) from
    y = z0 to z1."""
    if area(poly) < 0:
        poly = poly[::-1]
    n = len(poly)
    def p(i, h):
        x, y = poly[i % n]
        return (x, y, h) if plane == "xy" else (x, h, y)
    tris = []
    for a, b, c in ear_clip(poly):
        tris.append((p(a, z0), p(c, z0), p(b, z0)))
        tris.append((p(a, z1), p(b, z1), p(c, z1)))
    for i in range(n):
        tris.append((p(i, z0), p(i + 1, z0), p(i + 1, z1)))
        tris.append((p(i, z0), p(i + 1, z1), p(i, z1)))
    if plane == "xz":
        # Swapping y and z mirrors the solid: turn every triangle round.
        tris = [(a, c, b) for a, b, c in tris]
    return tris


def box(x0, y0, z0, x1, y1, z1):
    return prism([(x0, y0), (x1, y0), (x1, y1), (x0, y1)], z0, z1)


def normal(a, b, c):
    u = [b[k] - a[k] for k in range(3)]
    w = [c[k] - a[k] for k in range(3)]
    n = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]]
    length = sum(x * x for x in n) ** 0.5 or 1.0
    return [x / length for x in n]


def write(path, tris):
    # With zero normals OrcaSlicer 2.4 refuses some meshes ("Loading of a model file failed").
    with open(path, "wb") as f:
        f.write(bytes(80) + struct.pack("<I", len(tris)))
        for a, b, c in tris:
            f.write(struct.pack("<12fH", *normal(a, b, c), *a, *b, *c, 0))


def models():
    yield "cube20", box(0, 0, 0, 20, 20, 20)
    yield "small", box(0, 0, 0, 5, 5, 5)
    yield "large", box(0, 0, 0, 150, 100, 3)
    circle = [(15 * math.cos(2 * math.pi * k / 128), 15 * math.sin(2 * math.pi * k / 128)) for k in range(128)]
    yield "cylinder", prism(circle, 0, 20)
    # Fins on a base as one outline (a comb), so the model is one closed shell.
    comb, x = [(0, 0), (60, 0), (60, 2)], 60.0
    fins = [1.6, 1.2, 1.0, 0.8, 0.6, 0.45, 0.3]
    xs = []
    for k, t in enumerate(fins):
        x0 = 4 + k * 8
        xs.append((x0, x0 + t))
    for x0, x1 in reversed(xs):
        comb += [(x1, 2), (x1, 12), (x0, 12), (x0, 2)]
    comb += [(0, 2)]
    yield "thinwalls", prism(comb, 0, 15, plane="xz")
    tris = []
    for k, deg in enumerate([20, 35, 45, 55, 70]):
        x0, h = k * 22, 10.0
        lean = h * math.tan(math.radians(deg))
        tris += prism([(x0, 0), (x0 + 6, 0), (x0 + 6 + lean, h), (x0, h)], 0, 10, plane="xz")
    # Separate shells, spaced so they never touch (the last, at 70 degrees, leans 27 mm).
    yield "overhangs", tris
    yield "bridge", prism([(0, 0), (5, 0), (5, 10), (25, 10), (25, 0), (30, 0), (30, 12), (0, 12)], 0, 10, plane="xz")
    tris = []
    for k in range(6):
        s = 30 - 5 * k
        tris += box(-s / 2, -s / 2, 3 * k, s / 2, s / 2, 3 * k + 3)
    yield "steps", tris


if __name__ == "__main__":
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    for name, tris in models():
        write(os.path.join(out, name + ".stl"), tris)
        print(f"{name}: {len(tris)} triangles")
