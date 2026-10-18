# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare completed spike-base crossings with exact half-open geometry."""

import argparse
import ctypes as C
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile


def intersects(x, y, dx, dy, bounds):
    low, high, low_open, high_open = Fraction(0), Fraction(1), False, False
    for start, delta, left, right in [(x, dx, bounds[0], bounds[1]), (y, dy, bounds[2], bounds[3])]:
        if not delta:
            if not left <= start < right:
                return False
            continue
        for value, lower, opened in [(Fraction(left - start, delta), delta > 0, False),
                                     (Fraction(right - start, delta), delta < 0, True)]:
            if lower:
                if value > low:
                    low, low_open = value, opened
                elif value == low:
                    low_open |= opened
            else:
                if value < high:
                    high, high_open = value, opened
                elif value == high:
                    high_open |= opened
    return low < high or (low == high and not low_open and not high_open)


def run(source, review_oracle=None):
    header = source / "SAUCE/functions/base_crossing.h"
    program = r'''
#include <stdint.h>
#define CODE_BANK_PUSH(x)
#define CODE_BANK_POP()
#define crossPRGBankJump0(fn) fn()
unsigned char ss_type, ss_player, cube_data[2], ss_abs_dx, ss_abs_dy;
signed char ss_origin_x, ss_origin_y, ss_dx, ss_dy, ss_x0, ss_x1, ss_y0, ss_y1, ss_swap;
#include "SAUCE/functions/base_crossing.h"
const unsigned char metatiles_bases[128] = {0};
unsigned char actual(unsigned char kind, signed char x, signed char y, signed char dx, signed char dy) {
    ss_type=kind; ss_origin_x=-x; ss_origin_y=-y; ss_dx=dx; ss_dy=dy;
#ifdef BASE_METADATA
    ss_base_axis=kind<66 ? ((kind-62)&1)^1 : (kind&1)+2;
#endif
    return ss_base_hit();
}
'''
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "oracle.c").write_text(program)
        subprocess.run(["cc", "-shared", "-fPIC", "-O2", "-I", str(source), str(root / "oracle.c"),
                        "-o", str(root / "oracle.so")], check=True, capture_output=True)
        library = C.CDLL(str(root / "oracle.so"))
        actual = library.actual
        actual.argtypes, actual.restype = [C.c_uint8] + [C.c_int8] * 4, C.c_uint8
        cases = 0
        expected_bytes = bytearray() if review_oracle else None
        deltas = [-16, -8, -5, -4, -3, -1, 0, 1, 3, 4, 5, 8, 16]
        for kind, bounds, axis in [(62, (0, 16, 15, 16), 1), (63, (0, 16, 0, 1), 1),
                                    (66, (0, 1, 0, 16), 0), (67, (15, 16, 0, 16), 0)]:
            for x in range(-16, 32):
                for y in range(-16, 32):
                    for dx in deltas:
                        for dy in deltas:
                            start, end = (y, y + dy) if axis else (x, x + dx)
                            full = min(start, end) <= bounds[axis * 2] and max(start, end) >= bounds[axis * 2 + 1]
                            expected = full and intersects(x, y, dx, dy, bounds)
                            observed = bool(actual(kind, x, y, dx, dy))
                            assert observed == expected, (kind, x, y, dx, dy, observed, expected)
                            if expected_bytes is not None:
                                expected_bytes.append(expected)
                            cases += 1
        for kind, alias in [(64, 62), (65, 63), (68, 66), (69, 67)]:
            for x in range(16):
                for y in range(16):
                    for dx in (-16, -1, 0, 1, 16):
                        for dy in (-16, -1, 0, 1, 16):
                            assert actual(kind, x, y, dx, dy) == actual(alias, x, y, dx, dy)
                            cases += 1
    if review_oracle:
        review_oracle.write_bytes(expected_bytes)
    return {"status": "completed-spike-bases-match-exact-geometry", "cases": cases,
            "orientations": 4, "source_sha256": hashlib.sha256(header.read_bytes()).hexdigest()}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=lambda value: Path(value).resolve())
    parser.add_argument("--review-oracle", type=Path)
    args = parser.parse_args()
    print(json.dumps(run(args.source, args.review_oracle)))
