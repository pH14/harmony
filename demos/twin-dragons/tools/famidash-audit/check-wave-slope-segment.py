# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check the experimental continuous 45-degree slope kernel against rational clipping."""

import argparse
import ctypes as C
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import random
import subprocess
import tempfile


NAMES = ["COL_SLOPE_RD45", "COL_SLOPE_LD45", "COL_SLOPE_RU45", "COL_SLOPE_LU45"]


def oracle(dx, dy, ox, oy, kind):
    planes = [(-1, 0, -ox), (1, 0, ox + 16), (0, -1, -oy), (0, 1, oy + 16)]
    planes.append([
        (-1, -1, -ox - oy - 16), (1, -1, ox - oy),
        (-1, 1, oy - ox), (1, 1, ox + oy + 16),
    ][kind - 1])
    low, high = Fraction(0), Fraction(1)
    for x, y, limit in planes:
        velocity = x * dx + y * dy
        if not velocity:
            if limit < 0:
                return False
        elif velocity > 0:
            high = min(high, Fraction(limit, velocity))
        else:
            low = max(low, Fraction(limit, velocity))
    return low <= high


def cases():
    yield 4, -4, -12, -19, 3
    yield 4, -4, -12, -19, 1
    yield 0, 0, 0, 0, 4
    for kind in range(1, 5):
        for dx in range(-16, 17):
            for dy in range(-16, 17):
                for ox, oy in [(0, 0), (-16, -16), (-16, 0), (0, -16),
                               (-12, -19), (dx - 16, dy - 16)]:
                    yield dx, dy, ox, oy, kind
    rng = random.Random(790)
    for _ in range(20000):
        yield (rng.randrange(-16, 17), rng.randrange(-16, 17),
               rng.randrange(-31, 17), rng.randrange(-31, 17), rng.randrange(1, 5))


def check(source):
    path = source / "SAUCE/functions/spike_sweep.h"
    text = path.read_text()
    start = text.index("int wave_signed_product(")
    end = text.index("void wave_test_slope_tile", start)
    kernel = text[start:end]
    assert "wave_triangle_hit(void)" in kernel
    prefix = "\n".join(f"#define {name} {index + 1}" for index, name in enumerate(NAMES))
    wrapper = """
signed char ss_dx, ss_dy, ss_origin_x, ss_origin_y;
unsigned char collision;
"""
    wrapper += kernel
    wrapper += """
unsigned char verify_wrapper(signed char dx, signed char dy, signed char ox,
                             signed char oy, unsigned char type) {
    ss_dx = dx; ss_dy = dy; ss_origin_x = ox; ss_origin_y = oy; collision = type;
    return wave_triangle_hit();
}
"""
    geometry_cases = product_cases = 0
    with tempfile.TemporaryDirectory() as directory:
        build = Path(directory)
        (build / "kernel.c").write_text(prefix + "\n" + wrapper)
        subprocess.run(["cc", "-shared", "-fPIC", "-O2", str(build / "kernel.c"),
                        "-o", str(build / "kernel.so")], check=True)
        library = C.CDLL(str(build / "kernel.so"))
        product = library.wave_signed_product
        product.argtypes = [C.c_byte, C.c_byte]
        product.restype = C.c_int
        for x in range(-128, 128):
            for y in range(-128, 128):
                actual = product(x, y)
                assert actual == x * y, (x, y, actual)
                product_cases += 1
        hit = library.verify_wrapper
        hit.argtypes = [C.c_byte] * 4 + [C.c_ubyte]
        hit.restype = C.c_ubyte
        for case in cases():
            actual, expected = bool(hit(*case)), oracle(*case)
            assert actual == expected, (case, actual, expected)
            geometry_cases += 1
    return {
        "status": "verified", "cases": geometry_cases,
        "signed_product_cases": product_cases,
        "source_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        "reference": "Independent exact rational clipping against five half-planes; outer tile boundaries are 0..16.",
        "limitation": "Host arithmetic and geometry only; NES mapping, dispatch, pixel intent and frame budget require separate checks.",
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = check(args.source)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
