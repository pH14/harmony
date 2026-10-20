# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check the actual mini-wave45 kernel against source points and rational clipping."""

import argparse
import ctypes as C
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import random
import re
import subprocess
import tempfile


NAMES = ["RD45", "LD45", "RU45", "LU45"]
DIAGONALS = [(-1, -1, -16), (1, -1, 0), (-1, 1, 0), (1, 1, 16)]


def oracle(dx, dy, ox, oy, kind):
    left, top = Fraction(2 * ox - 1, 2), Fraction(2 * oy - 1, 2)
    a, b, limit = DIAGONALS[kind]
    planes = [(-1, 0, -left), (1, 0, left + 16), (0, -1, -top), (0, 1, top + 16),
              (a, b, limit + a * left + b * top)]
    lower, upper = Fraction(0), Fraction(1)
    for a, b, limit in planes:
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
    yield 4, 7, -12, 6, 0
    yield -4, -7, -16, -1, 0
    for kind in range(4):
        for dx in range(-16, 17):
            for dy in range(-16, 17):
                for ox, oy in [(0, 0), (-16, -16), (-12, 6), (dx - 16, dy - 16)]:
                    yield dx, dy, ox, oy, kind
    rng = random.Random(3893)
    for _ in range(30000):
        yield (rng.randrange(-16, 17), rng.randrange(-16, 17), rng.randrange(-31, 17),
               rng.randrange(-31, 17), rng.randrange(4))


def source_points(collision, names=NAMES):
    start = re.search(r'char\s+bg_coll_slope\s*\(\s*\)\s*\{', collision).start()
    text = collision[start:]
    blocks = []
    for name in names:
        marker = "col_slope_" + name + ":"
        begin = text.index(marker)
        end = re.search(r'\n\s*col_\w+:', text[begin + len(marker):]).start()
        blocks.append(text[begin:begin + len(marker) + end])
    prefix = """
#define GAMEMODE_WAVE 6
#define GAMEMODE_SNAKE 9
#define SLOPE_45DEG_UP 0
#define SLOPE_45DEG_DOWN 0
#define SLOPE_45DEG_UP_UD 0
#define SLOPE_45DEG_DOWN_UD 0
static unsigned char temp_x, temp_y, tmp4, tmp7, currplayer_slope_type;
static unsigned char gamemode = GAMEMODE_WAVE, currplayer_mini = 1;
unsigned char original_point(unsigned char kind, unsigned char x, unsigned char y) {
    temp_x = x; temp_y = y;
    switch (kind) {
"""
    jumps = "\n".join(f"case {n}: goto col_slope_{name};" for n, name in enumerate(names))
    return prefix + jumps + "\n}\n" + "\n".join(blocks) + "\ncol_end: return tmp4 >= tmp7;\n}\n"


def check(source):
    header = source / "SAUCE/functions/mini_wave45.h"
    text = header.read_text()
    start = text.index("static signed char m45_dx")
    end = text.index("\n}\n", text.index("unsigned char mini45_triangle_hit(void)", start)) + 3
    kernel = text[start:end]
    definitions = "\n".join(f"#define COL_SLOPE_{name} {n}" for n, name in enumerate(NAMES))
    wrapper = """
unsigned char verify(signed char dx, signed char dy, signed char ox,
                     signed char oy, unsigned char kind) {
    m45_dx = dx * 2; m45_dy = dy * 2;
    m45_ox = ox * 2 - 1; m45_oy = oy * 2 - 1;
    m45_type = kind;
    return mini45_triangle_hit();
}
"""
    point_source = source_points((source / "SAUCE/functions/collision.h").read_text())
    geometry_cases, point_cases, product_cases = 0, 0, 0
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory)
        (path / "kernel.c").write_text(definitions + "\n" + kernel + wrapper + point_source)
        subprocess.run(["cc", "-shared", "-fPIC", "-O2", str(path / "kernel.c"), "-o", str(path / "kernel.so")], check=True)
        library = C.CDLL(str(path / "kernel.so"))
        actual = library.verify
        actual.argtypes, actual.restype = [C.c_int8] * 4 + [C.c_uint8], C.c_uint8
        point = library.original_point
        point.argtypes, point.restype = [C.c_uint8] * 3, C.c_uint8
        product = library.m45_signed_product
        product.argtypes, product.restype = [C.c_int8] * 2, C.c_int16
        for dx, dy, ox, oy, kind in cases():
            expected = oracle(dx, dy, ox, oy, kind)
            assert bool(actual(dx, dy, ox, oy, kind)) == expected, (dx, dy, ox, oy, kind)
            geometry_cases += 1
        for kind in range(4):
            for x in range(16):
                for y in range(16):
                    assert actual(0, 0, -x, -y, kind) == point(kind, x, y), (kind, x, y)
                    point_cases += 1
        for x in range(-128, 128):
            for y in range(-128, 128):
                assert product(x, y) == x * y, (x, y)
                product_cases += 1
    return {"status": "verified", "geometry_cases": geometry_cases, "source_point_cases": point_cases,
            "signed_product_cases": product_cases, "source_sha256": hashlib.sha256(text.encode()).hexdigest(),
            "point_source_sha256": hashlib.sha256(point_source.encode()).hexdigest(),
            "coordinate_convention": "Pixel centres, scaled by two; both origins subtract one half pixel.",
            "limitation": "Host geometry and actual source-point predicates; NES dispatch, history, banking, timing and route intent require replay checks."}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = check(args.source)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
