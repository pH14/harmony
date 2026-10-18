# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare the proposed C pit guard with an exact geometric oracle."""

import argparse
import ctypes as C
from fractions import Fraction
import hashlib
import itertools
import json
from pathlib import Path
import subprocess
import tempfile


def intersects(old_x, new_x, old_y, new_y, cell):
    low, high, low_open, high_open = Fraction(0), Fraction(1), False, False
    for start, end, left, right in [(old_x, new_x, cell * 16, cell * 16 + 16),
                                    (old_y, new_y, -1, 0)]:
        delta = end - start
        if not delta:
            if not left <= start < right:
                return False
            continue
        bounds = [(Fraction(left - start, delta), delta > 0, False),
                  (Fraction(right - start, delta), delta < 0, True)]
        for value, lower, opened in bounds:
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


def run(source):
    header = source.resolve() / "SAUCE/functions/pit_crossing.h"
    program = r'''
#include <stdint.h>
#define CODE_BANK_PUSH(x)
#define CODE_BANK_POP()
#define __asm__(...)
#define crossPRGBankJump0(fn) fn()
#define COL_PIT 62
uint8_t temp_x, temp_y, temp_room, collision, currplayer;
uint8_t cube_data[2], collision_teleported[2], wrap_mode;
static uint8_t current_is_pit;
void bg_collision_sub(void) { collision = current_is_pit ? COL_PIT : 0; }
'''
    program += "#include " + json.dumps(str(header)) + "\n"
    program += r'''
int check(int old_x, int new_x, int delta, int local_y, int old_is_pit, int new_is_pit) {
    temp_x = new_x; temp_y = local_y; temp_room = 1; collision = 0;
    currplayer = pit_player = 0; cube_data[0] = 0;
    pit_old_x = old_x; pit_delta = delta; pit_old = old_is_pit; current_is_pit = new_is_pit;
    pit_low_y = 16 + local_y - delta; pit_low_room = 1;
    pit_high_x = new_x; pit_high_y = 16 + local_y;
    if ((old_x & 240) == (new_x & 240)) return old_is_pit;
    check_pit_column();
    return cube_data[0] & 1;
}
int check_guard(int old_x, int new_x, int delta, int local_y, int old_is_pit, int new_is_pit,
                int upwards, int room_wrap) {
    int low_y = (room_wrap ? 240 : 16) + local_y - delta;
    int high_y = room_wrap ? local_y : 16 + local_y;
    int high_room = room_wrap ? 2 : 1;
    currplayer = 0; cube_data[0] = 0; collision_teleported[0] = wrap_mode = 0;
    current_is_pit = new_is_pit;
    pit_previous_x[0] = upwards ? new_x : old_x;
    pit_previous_y[0] = upwards ? high_y : low_y;
    pit_previous_room[0] = upwards ? high_room : 1;
    pit_previous_collision[0] = upwards ? 0 : (old_is_pit ? COL_PIT : 0);
    temp_x = upwards ? old_x : new_x; temp_y = upwards ? low_y : high_y;
    temp_room = upwards ? 1 : high_room;
    collision = upwards && old_is_pit ? COL_PIT : 0;
    check_pit_motion();
    return cube_data[0] & 1;
}
'''
    cases = 0
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory)
        (path / "check.c").write_text(program)
        subprocess.run(["cc", "-std=c99", "-shared", "-fPIC", "-O2", str(path / "check.c"),
                        "-o", str(path / "check.so")], check=True)
        check = C.CDLL(str(path / "check.so")).check
        check.argtypes, check.restype = [C.c_int] * 6, C.c_int
        guard = C.CDLL(str(path / "check.so")).check_guard
        guard.argtypes, guard.restype = [C.c_int] * 8, C.c_int
        for old_x, dx, delta, old_pit, new_pit in itertools.product(
                range(16), range(-16, 17), range(1, 17), range(2), range(2)):
            new_x = old_x + dx
            if old_x // 16 == new_x // 16:
                continue
            for local_y in range(delta):
                expected = ((old_pit and intersects(old_x, new_x, local_y - delta, local_y, old_x // 16))
                            or (new_pit and intersects(old_x, new_x, local_y - delta, local_y, new_x // 16)))
                actual = bool(check(old_x, new_x & 255, delta, local_y, old_pit, new_pit))
                assert actual == bool(expected), (old_x, new_x, delta, local_y, old_pit, new_pit, expected, actual)
                cases += 1
                for upwards, room_wrap in itertools.product(range(2), range(2)):
                    actual = bool(guard(old_x, new_x & 255, delta, local_y, old_pit, new_pit, upwards, room_wrap))
                    assert actual == bool(expected), ("guard", old_x, new_x, delta, local_y, old_pit, new_pit,
                                                      upwards, room_wrap, expected, actual)
                    cases += 1
    print(json.dumps({"status": "exact-geometric-oracle-matches", "cases": cases,
                      "source_sha256": hashlib.sha256(header.read_bytes()).hexdigest()}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    run(parser.parse_args().source)
