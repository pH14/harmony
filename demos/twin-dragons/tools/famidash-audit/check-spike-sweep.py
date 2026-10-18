# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare the proposed C sweep with original C hitboxes and exact segment geometry."""

import argparse
import ctypes as C
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile


def extract(text, name):
    start = text.index(f"char {name}(")
    opening = text.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start:end]


def intersects(x, y, dx, dy, rectangle):
    low, high = Fraction(0), Fraction(1)
    for origin, delta, left, right in ((x, dx, rectangle[0], rectangle[1]),
                                       (y, dy, rectangle[2], rectangle[3])):
        if not delta:
            if not left <= origin <= right:
                return False
        else:
            a, b = sorted((Fraction(left - origin, delta), Fraction(right - origin, delta)))
            low, high = max(low, a), min(high, b)
    return low <= high


def rectangles(points):
    active, result = {}, []
    for y in range(17):
        runs, start = [], None
        for x in range(17):
            hit = x < 16 and y < 16 and (x, y) in points
            if hit and start is None:
                start = x
            if not hit and start is not None:
                runs.append((start, x - 1))
                start = None
        for run in list(active):
            if run not in runs:
                result.append((*run, active.pop(run), y - 1))
        for run in runs:
            active.setdefault(run, y)
    return result


def run(source):
    header = source / "SAUCE/functions/spike_sweep.h"
    collision = (source / "SAUCE/functions/collision.h").read_text()
    functions = "\n".join(extract(collision, name) for name in
                          ["col_death_bottom_routine", "col_death_top_routine", "col_death_left_routine",
                           "col_death_right_routine", "bg_coll_spikes"])
    program = r'''
#include <stdint.h>
#define CODE_BANK_PUSH(x)
#define CODE_BANK_POP()
#define LSB(x) ((x) & 255)
#define do_if_in_range(v,a,b,code) do { if ((v)>=(a) && (v)<=(b)) code; } while(0)
#include "METATILES/metatiles.h"
unsigned char currplayer, wrap_mode, temp_x, temp_y, temp_room, collision, tmp2;
unsigned char cube_data[2], pit_previous_room[2], pit_previous_x[2], pit_previous_y[2], pit_previous_collision[2];
void bg_collision_sub(void) {}
void check_pit_motion(void) {}
#include "SAUCE/functions/spike_sweep.h"
'''
    program += functions
    program += r'''
unsigned char original(unsigned char kind, unsigned char x, unsigned char y) {
    currplayer=0; cube_data[0]=0; collision=kind; temp_x=x; temp_y=y;
    return !!bg_coll_spikes();
}
unsigned char swept(unsigned char kind, unsigned char x, unsigned char y, signed char dx, signed char dy) {
    FAST_REJECT
    ss_player=0; cube_data[0]=0; ss_type=kind; ss_origin_x=-x; ss_origin_y=-y; ss_dx=dx; ss_dy=dy;
    ss_test_tile(); return !!(cube_data[0]&1);
}
'''
    fast_reject = ""
    if "ss_same_tile_clear" in header.read_text():
        fast_reject = """
    currplayer=0; temp_x=x+dx; temp_y=y+dy; temp_room=0; collision=kind;
    pit_previous_x[0]=x; pit_previous_y[0]=y; pit_previous_room[0]=0; pit_previous_collision[0]=kind;
    if (ss_shapes[kind] && !(((temp_x ^ x) | (temp_y ^ y)) & 240) && ss_same_tile_clear()) return 0;
"""
    program = program.replace("FAST_REJECT", fast_reject)
    if "#define SPIKE_LINEAR_Y 1" in header.read_text():
        program = program.replace("unsigned char currplayer, wrap_mode, temp_x, temp_y, temp_room, collision, tmp2;",
                                  "unsigned char currplayer, wrap_mode, temp_x, temp_room, collision, tmp2; unsigned short temp_y;")
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        (root / "oracle.c").write_text(program)
        subprocess.run(["cc", "-shared", "-fPIC", "-O2", "-I", str(source), str(root / "oracle.c"),
                        "-o", str(root / "oracle.so")], check=True, capture_output=True)
        library = C.CDLL(str(root / "oracle.so"))
        library.original.argtypes = [C.c_uint8] * 3
        library.swept.argtypes = [C.c_uint8] * 3 + [C.c_int8] * 2
        library.original.restype = library.swept.restype = C.c_uint8
        kinds, point_cases, sweep_cases = {}, 0, 0
        for kind in range(64):
            points = set()
            for x in range(16):
                for y in range(16):
                    expected = library.original(kind, x, y)
                    assert library.swept(kind, x, y, 0, 0) == expected, (kind, x, y)
                    point_cases += 1
                    if expected:
                        points.add((x, y))
            if points:
                kinds.setdefault(frozenset(points), kind)
        deltas = [-16, -8, -5, -4, -3, -1, 0, 1, 3, 4, 5, 8, 16]
        for points, kind in kinds.items():
            bounds = rectangles(points)
            for x in range(16):
                for y in range(16):
                    for dx in deltas:
                        for dy in deltas:
                            expected = any(intersects(x, y, dx, dy, bound) for bound in bounds)
                            actual = bool(library.swept(kind, x, y, dx, dy))
                            assert actual == expected, (kind, x, y, dx, dy, bounds, actual, expected)
                            sweep_cases += 1
        return {"status": "original-hitboxes-and-geometric-oracle-match", "point_cases": point_cases,
                "sweep_cases": sweep_cases, "distinct_shapes": len(kinds),
                "source_sha256": hashlib.sha256(header.read_bytes()).hexdigest()}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=lambda value: Path(value).resolve())
    args = parser.parse_args()
    print(json.dumps(run(args.source)))
