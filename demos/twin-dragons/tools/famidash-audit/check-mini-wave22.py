# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check conservative mini-wave22 motion without enlarging source point masks."""

import argparse
import ctypes as C
from fractions import Fraction
import hashlib
import importlib.util
import json
from pathlib import Path
import random
import subprocess
import tempfile

spec = importlib.util.spec_from_file_location("mini45_oracle", Path(__file__).with_name("check-mini-wave45.py"))
source_helpers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source_helpers)

NAMES = ["RU22_RIGHT", "RU22_LEFT", "RD22_RIGHT", "RD22_LEFT",
         "LU22_RIGHT", "LU22_LEFT", "LD22_RIGHT", "LD22_LEFT"]
PLANES = [(-1, 2, 0), (-1, 2, 16), (-1, -2, -32), (-1, -2, -16),
          (1, 2, 32), (1, 2, 16), (1, -2, 0), (1, -2, -16)]


def oracle(dx, dy, ox, oy, kind):
    left, top = Fraction(2 * ox - 1, 2), Fraction(2 * oy - 1, 2)
    a, b, limit = PLANES[kind]
    lower, upper = Fraction(0), Fraction(1)
    for a, b, limit in [(-1, 0, -left), (1, 0, left + 16),
                        (0, -1, -top), (0, 1, top + 16),
                        (a, b, limit + a * left + b * top)]:
        velocity = a * dx + b * dy
        if not velocity:
            if limit < 0:
                return False
        elif velocity > 0:
            upper = min(upper, Fraction(limit, velocity))
        else:
            lower = max(lower, Fraction(limit, velocity))
    return lower <= upper


def cases():
    yield 3, 7, -13, 5, 3
    yield -3, -7, -16, -2, 3
    for kind in range(8):
        for dx in range(-16, 17):
            for dy in range(-16, 17):
                for ox, oy in [(0, 0), (-16, -16), (-13, 5), (dx - 16, dy - 16)]:
                    yield dx, dy, ox, oy, kind
    rng = random.Random(1844)
    for _ in range(40000):
        yield (rng.randrange(-16, 17), rng.randrange(-16, 17), rng.randrange(-31, 17),
               rng.randrange(-31, 17), rng.randrange(8))


def check(source):
    header = source / "SAUCE/functions/mini_wave22.h"
    text = header.read_text()
    start = text.index("static signed char m22_dx")
    end = text.index("\n}\n", text.index("unsigned char mini22_polygon_hit(void)", start)) + 3
    kernel = text[start:end]
    definitions = "\n".join(f"#define COL_SLOPE_{name} {n}" for n, name in enumerate(NAMES))
    wrapper = """
unsigned char verify(signed char dx, signed char dy, signed char ox,
                     signed char oy, unsigned char kind) {
    m22_dx = dx * 2; m22_dy = dy * 2;
    m22_ox = ox * 2 - 1; m22_oy = oy * 2 - 1;
    m22_type = kind;
    return mini22_polygon_hit();
}
"""
    point_source = "\n".join(f"#define SLOPE_22DEG_{name} 0" for name in ["UP", "DOWN", "UP_UD", "DOWN_UD"])
    point_source += "\n" + source_helpers.source_points((source / "SAUCE/functions/collision.h").read_text(), NAMES)
    geometry_cases = point_cases = product_cases = conservative_boundary_points = 0
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory)
        (path / "kernel.c").write_text(definitions + "\n" + kernel + wrapper + point_source)
        subprocess.run(["cc", "-shared", "-fPIC", "-O2", str(path / "kernel.c"), "-o", str(path / "kernel.so")], check=True)
        library = C.CDLL(str(path / "kernel.so"))
        actual = library.verify
        actual.argtypes, actual.restype = [C.c_int8] * 4 + [C.c_uint8], C.c_uint8
        point = library.original_point
        point.argtypes, point.restype = [C.c_uint8] * 3, C.c_uint8
        product = library.m22_signed_product
        product.argtypes, product.restype = [C.c_int8] * 2, C.c_int16
        for dx, dy, ox, oy, kind in cases():
            assert bool(actual(dx, dy, ox, oy, kind)) == oracle(dx, dy, ox, oy, kind), (dx, dy, ox, oy, kind)
            geometry_cases += 1
        for kind in range(8):
            for x in range(16):
                for y in range(16):
                    swept, original = actual(0, 0, -x, -y, kind), point(kind, x, y)
                    assert not swept or original, ("stationary hitbox expanded", kind, x, y)
                    assert (swept | original) == original
                    conservative_boundary_points += bool(original and not swept)
                    point_cases += 1
        for x in range(-128, 128):
            for y in range(-128, 128):
                assert product(x, y) == x * y
                product_cases += 1
    return {"status": "verified", "geometry_cases": geometry_cases,
            "source_point_preservation_cases": point_cases, "signed_product_cases": product_cases,
            "conservative_boundary_points": conservative_boundary_points,
            "source_sha256": hashlib.sha256(text.encode()).hexdigest(),
            "point_source_sha256": hashlib.sha256(point_source.encode()).hexdigest(),
            "coordinate_convention": "Pixel centres and conservative visible polygons; original discrete point checks remain active.",
            "invariant": "The sweep's stationary mask is a subset of the original mask. Their union preserves every original point result.",
            "limitation": "Host geometry only; NES banking, history, unchanged-input negatives and legitimate routes require independent replay."}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = check(args.source)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
