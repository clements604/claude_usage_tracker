"""Report surface on an STL that would need support when printed as-is.

Counts every triangle that faces downward more steeply than 45 degrees and is
not on the bed (z = 0). Flat ceilings (bridges) count too. Standard library
only, so it runs without installing anything.

Usage: python3 check_overhangs.py FILE.stl...   (exit status 1 if any overhang)
"""

import math
import struct
import sys


def triangles(path):
    data = open(path, "rb").read()
    if len(data) >= 84:
        n = struct.unpack_from("<I", data, 80)[0]
        if len(data) == 84 + 50 * n:
            for i in range(n):
                f = struct.unpack_from("<12f", data, 84 + 50 * i)
                yield f[3:6], f[6:9], f[9:12]
            return
    verts = []
    for line in data.decode().splitlines():
        parts = line.split()
        if parts and parts[0] == "vertex":
            verts.append(tuple(map(float, parts[1:4])))
            if len(verts) == 3:
                yield tuple(verts)
                verts = []


def overhang_area(path, max_deg=45.0):
    limit = -math.sin(math.radians(max_deg)) - 1e-3
    zmin = min(v[2] for t in triangles(path) for v in t)
    bad = 0.0
    for a, b, c in triangles(path):
        u = [b[i] - a[i] for i in range(3)]
        v = [c[i] - a[i] for i in range(3)]
        n = (u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0])
        length = math.sqrt(sum(x * x for x in n))
        if length < 2e-6:
            continue
        on_bed = max(a[2], b[2], c[2]) - zmin < 1e-3
        if n[2] / length < limit and not on_bed:
            bad += length / 2
    return bad


def main():
    worst = 0.0
    for path in sys.argv[1:]:
        area = overhang_area(path)
        worst = max(worst, area)
        print(f"{path}: unsupported overhang {area:.3f} mm^2")
    sys.exit(1 if worst > 0.01 else 0)


if __name__ == "__main__":
    main()
