# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check the aligned wave point and preservation of the existing probe state."""

import argparse
import ctypes as C
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile


def extract(text, name):
    match = re.search(r"(?:char|void)\s+" + name + r"\s*\([^;{}]*\)\s*\{", text)
    assert match
    opening = text.index("{", match.start())
    depth = 0
    for token in re.finditer(r'/\*[\s\S]*?\*/|//[^\n]*|"(?:\\.|[^"\\])*"|[{}]', text[opening:]):
        if token[0] not in ("{", "}"):
            continue
        depth += 1 if token[0] == "{" else -1
        if not depth:
            return text[match.start():opening + token.end()]
    raise AssertionError("unterminated function")


def run(original, repaired):
    before = (original / "SAUCE/functions/collision.h").read_text()
    after = (repaired / "SAUCE/functions/collision.h").read_text()
    assert extract(before, "bg_coll_slope") == extract(after, "bg_coll_slope"), "body slope routine changed"
    definitions = dict((name, int(value, 0)) for name, value in re.findall(
        r"^#define (COL_\w+)\s+(0x[\da-fA-F]+|\d+)", (repaired / "METATILES/metatiles.h").read_text(), re.M))
    orientations = ("RD45", "LD45", "RU45", "LU45") if "void wave_slope_point(" in after else ("LU45",)
    kinds = (0, definitions["COL_ALL"], *(definitions["COL_SLOPE_" + name] for name in orientations))
    helper = extract(after, "wave_slope_center")
    if len(orientations) > 1:
        helper = extract(after, "wave_slope_point") + "\n" + helper
    linear = "temp_room" not in extract(after, "bg_coll_death")
    y_limit = 960 if linear else 240
    program = r'''
#include <stdint.h>
#include "METATILES/metatiles.h"
unsigned char temp_x,temp_y,temp_room,collision,collision_metatile,currplayer,cube_data[2];
unsigned char grid[16];
char bg_collision_sub(void) {
    if (temp_y>=240) {collision=0;return 0;}
    collision=grid[temp_x>>4];collision_metatile=(temp_x>>4)+17;return collision;
}
'''
    program += helper
    program += r'''
uint64_t evaluate(unsigned char x,unsigned char y,unsigned char oldkind,
                  unsigned char nextkind,unsigned char flags,unsigned char player) {
    temp_x=x;temp_y=y;temp_room=2;currplayer=player;
    cube_data[player]=flags;cube_data[player^1]=99;
    grid[x>>4]=oldkind;grid[((x>>4)+1)&15]=nextkind;
    collision=y<240?oldkind:0;collision_metatile=y<240?(x>>4)+17:221;
    wave_slope_center();
    return (uint64_t)cube_data[player]|((uint64_t)temp_x<<8)|((uint64_t)temp_y<<16)|
           ((uint64_t)collision<<32)|((uint64_t)collision_metatile<<40)|
           ((uint64_t)temp_room<<48)|((uint64_t)cube_data[player^1]<<56);
}
'''
    if linear:
        program = program.replace("unsigned char temp_x,temp_y,temp_room,", "unsigned short temp_y;\nunsigned char temp_x,temp_room,")
        program = program.replace("temp_y>=240", "temp_y>=960").replace("y<240", "y<960")
        program = program.replace("unsigned char x,unsigned char y,unsigned char oldkind", "unsigned char x,unsigned short y,unsigned char oldkind")
    with tempfile.TemporaryDirectory(prefix="famidash-wave-point-") as temporary:
        root = Path(temporary)
        (root / "point.c").write_text(program)
        library = root / "point.so"
        subprocess.run(["cc", "-shared", "-fPIC", "-O2", "-I", str(repaired), str(root / "point.c"), "-o", str(library)], check=True)
        core = C.CDLL(str(library))
        core.evaluate.argtypes = [C.c_ubyte, C.c_ushort if linear else C.c_ubyte, *([C.c_ubyte] * 4)]
        core.evaluate.restype = C.c_uint64
        cases = 0
        for oldkind in kinds:
            for nextkind in kinds:
                for y in range(960 if linear else 256):
                    for x in range(256):
                        kind = oldkind if x & 15 < 12 else nextkind
                        px, py = (x + 4) & 15, y & 15
                        regions = {"RD45": px + py >= 15, "LD45": py >= px,
                                   "RU45": py <= px, "LU45": px + py <= 15}
                        hit = y < y_limit and any(kind == definitions["COL_SLOPE_" + name] and regions[name]
                                             for name in orientations)
                        for flags in (0, 2, 128):
                            for player in (0, 1):
                                expected = ((flags | hit) | (x << 8) | (y << 16) |
                                            ((oldkind if y < y_limit else 0) << 32) |
                                            (((x >> 4) + 17 if y < y_limit else 221) << 40) |
                                            (2 << 48) | (99 << 56))
                                assert core.evaluate(x, y, oldkind, nextkind, flags, player) == expected, (x, y, oldkind, nextkind, flags, player)
                                cases += 1
    return {"status": "aligned-wave-point-and-probe-state-preservation-verified", "cases": cases,
            "body_slope_routine": "source-identical to baseline", "orientations": orientations,
            "helper_sha256": hashlib.sha256(helper.encode()).hexdigest(),
            "source_sha256": hashlib.sha256(after.encode()).hexdigest(),
            "scope": "host-compiled helper; tile seams, both players, three incoming flag states",
            "coordinate_layout": "linear Y 0..959" if linear else "room-local byte Y including invalid 240..255",
            "limitation": "caller selection, sprite alignment and NES mapper require independent runtime validation"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("original", type=Path)
    parser.add_argument("repaired", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = run(args.original, args.repaired)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
