# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check the actual mini-wave66 polygon kernel against independent rational clipping."""

import argparse
import ctypes as C
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import random
import subprocess
import tempfile


NAMES = ["RD66_TOP", "LD66_TOP", "RD66_BOT", "LD66_BOT",
         "RU66_TOP", "LU66_TOP", "RU66_BOT", "LU66_BOT"]
DIAGONALS = [(-2, -1, -32), (2, -1, 0), (-2, -1, -16), (2, -1, 16),
             (-2, 1, -16), (2, 1, 16), (-2, 1, 0), (2, 1, 32)]


def oracle(dx, dy, ox, oy, kind):
    ax, ay, limit = DIAGONALS[kind]
    planes = [(-1, 0, -ox), (1, 0, ox + 16), (0, -1, -oy), (0, 1, oy + 16),
              (ax, ay, limit + ax * ox + ay * oy)]
    if kind in (0, 4):
        planes.append((-1, 0, -ox - 8))
    if kind in (1, 5):
        planes.append((1, 0, ox + 8))
    low, high = Fraction(0), Fraction(1)
    for ax, ay, limit in planes:
        velocity = ax * dx + ay * dy
        if not velocity:
            if limit < 0:
                return False
        elif velocity > 0:
            high = min(high, Fraction(limit, velocity))
        else:
            low = max(low, Fraction(limit, velocity))
    return low <= high


def cases():
    yield 5, -8, -12, -14, 4
    yield 5, 9, -13, -2, 0
    for kind in range(8):
        for dx in range(-16, 17):
            for dy in range(-16, 17):
                for ox, oy in [(0, 0), (-16, -16), (-12, -14), (dx - 16, dy - 16)]:
                    yield dx, dy, ox, oy, kind
    rng = random.Random(3064)
    for _ in range(30000):
        yield (rng.randrange(-16, 17), rng.randrange(-16, 17), rng.randrange(-31, 17),
               rng.randrange(-31, 17), rng.randrange(8))


def check(source):
    path = source / "SAUCE/functions/mini_wave66.h"
    text = path.read_text()
    start = text.index("static signed char mw_dx")
    end = text.index("unsigned char mw_valid[2];", start)
    kernel = text[start:end]
    prefix = "\n".join(f"#define COL_SLOPE_{name} {index}" for index, name in enumerate(NAMES))
    wrapper = """
unsigned char verify(signed char dx, signed char dy, signed char ox,
                     signed char oy, unsigned char type) {
    mw_dx = dx; mw_dy = dy; mw_ox = ox; mw_oy = oy; mw_type = type;
    return mini66_polygon_hit();
}
"""
    count = products = 0
    with tempfile.TemporaryDirectory() as directory:
        build = Path(directory)
        (build / "kernel.c").write_text(prefix + "\n" + kernel + wrapper)
        subprocess.run(["cc", "-shared", "-fPIC", "-O2", str(build / "kernel.c"),
                        "-o", str(build / "kernel.so")], check=True)
        library = C.CDLL(str(build / "kernel.so"))
        product = library.mw_signed_product
        product.argtypes = [C.c_byte, C.c_byte]
        product.restype = C.c_int
        for x in range(-128, 128):
            for y in range(-128, 128):
                actual = product(x, y)
                assert actual == x * y, (x, y, actual)
                products += 1
        hit = library.verify
        hit.argtypes = [C.c_byte] * 4 + [C.c_ubyte]
        hit.restype = C.c_ubyte
        for case in cases():
            actual, expected = bool(hit(*case)), oracle(*case)
            assert actual == expected, (case, actual, expected)
            count += 1
    return {"status": "verified", "geometry_cases": count, "signed_product_cases": products,
            "source_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "reference": "Independent exact rational clipping against rectangle, half-tile and diagonal planes.",
            "limitation": "Host arithmetic and geometry; history validity, NES mapping, pixel intent and frame budget require separate checks."}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = check(args.source)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
