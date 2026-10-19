# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check original body probes and exact slope points against the proposed C code."""

import argparse
import ctypes as C
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile


def function(text):
    match = re.search(r"char\s+bg_coll_slope\s*\(\s*(?:void)?\s*\)\s*\{", text)
    assert match
    opening = text.index("{", match.start())
    depth = 0
    for token in re.finditer(r'/\*[\s\S]*?\*/|//[^\n]*|"(?:\\.|[^"\\])*"|[{}]', text[opening:]):
        if token[0] not in ("{", "}"):
            continue
        depth += 1 if token[0] == "{" else -1
        if not depth:
            return text[match.start():opening + token.end()]
    raise AssertionError("unterminated slope function")


def compile_model(root, directory, name):
    source = (root / "SAUCE/functions/collision.h").read_text()
    modes = "\n".join(re.findall(r"^#define GAMEMODE_\w+\s+[^\n]+", (root / "SAUCE/famidash.h").read_text(), re.M))
    slopes = "\n".join(re.findall(r"^#define SLOPE_\w+\s+[^\n]+", source, re.M))
    program = r'''
#include <stdint.h>
#include "METATILES/metatiles.h"
#define PAD_A 1
#define PAD_UP 16
#define do_if_not_in_range(v,a,b,code) do {if ((v)<(a)||(v)>(b)) code;} while(0)
unsigned char collision,temp_x,temp_y,tmp4,tmp7,tmp8,gamemode,currplayer_mini;
unsigned char currplayer_slope_type,currplayer_slope_frames,currplayer_was_on_slope_counter;
unsigned char currplayer_gravity,make_cube_jump_higher,slope_center_probe;
unsigned char a_check_lookup[]={1,0,0,1,1,0,0,1};
struct Controller {unsigned char hold;};
struct Controller controller,*controllingplayer=&controller;
void unstick(void) {tmp8=4;}
'''
    program += modes + "\n" + slopes + "\n" + function(source)
    program += r'''
uint64_t evaluate(unsigned char kind,unsigned char x,unsigned char y,
                  unsigned char mode,unsigned char mini,unsigned char centre) {
    collision=kind;temp_x=x;temp_y=y;gamemode=mode;currplayer_mini=mini;slope_center_probe=centre;
    tmp4=tmp7=tmp8=currplayer_slope_type=currplayer_slope_frames=0;
    currplayer_was_on_slope_counter=make_cube_jump_higher=0;
    currplayer_gravity=x&1;controller.hold=(y&1)?PAD_A:0;
    unsigned char result=bg_coll_slope();
    return (uint64_t)result|((uint64_t)tmp8<<8)|((uint64_t)currplayer_slope_type<<16)|
           ((uint64_t)currplayer_slope_frames<<24)|((uint64_t)currplayer_was_on_slope_counter<<32)|
           ((uint64_t)make_cube_jump_higher<<40);
}
'''
    path = directory / (name + ".c")
    path.write_text(program)
    library = directory / (name + ".so")
    subprocess.run(["cc", "-shared", "-fPIC", "-O2", "-I", str(root), str(path), "-o", str(library)], check=True)
    core = C.CDLL(str(library))
    core.evaluate.argtypes = [C.c_ubyte] * 6
    core.evaluate.restype = C.c_uint64
    return core, source


def run(original, repaired):
    definitions = dict((name, int(value, 0)) for name, value in re.findall(
        r"^#define (COL_\w+)\s+(0x[\da-fA-F]+|\d+)", (repaired / "METATILES/metatiles.h").read_text(), re.M))
    with tempfile.TemporaryDirectory(prefix="famidash-slope-oracle-") as temporary:
        before, original_source = compile_model(original, Path(temporary), "before")
        after, repaired_source = compile_model(repaired, Path(temporary), "after")
        assert "!slope_center_probe" in repaired_source
        header = (repaired / "SAUCE/famidash.h").read_text()
        modes = dict((name, int(value, 0)) for name, value in re.findall(
            r"^#define (GAMEMODE_\w+)\s+(0x[\da-fA-F]+|\d+)", header, re.M))
        wave_modes = {modes["GAMEMODE_WAVE"]}
        if "gamemode == GAMEMODE_SNAKE" in function(repaired_source):
            wave_modes.add(modes["GAMEMODE_SNAKE"])
        body_cases = point_cases = 0
        for mode in set(modes.values()):
            for mini in (0, 1):
                for kind in range(256):
                    for y in range(16):
                        for x in range(16):
                            expected_body = before.evaluate(kind, x, y, mode, mini, 0)
                            assert after.evaluate(kind, x, y, mode, mini, 0) == expected_body, ("body changed", kind, x, y, mode, mini)
                            body_cases += 1
                            expected = expected_body & 255
                            if mode in wave_modes:
                                if kind == definitions["COL_SLOPE_LU45"] and not mini:
                                    expected = x + y <= 15
                                elif kind == definitions["COL_SLOPE_LU66_TOP"] and mini:
                                    expected = x < 8 and 2 * x + y <= 15
                                elif kind == definitions["COL_SLOPE_LU66_BOT"] and mini:
                                    expected = x < 8 or 2 * x + y <= 31
                            assert (after.evaluate(kind, x, y, mode, mini, 1) & 255) == expected, ("point mismatch", kind, x, y, mode, mini)
                            point_cases += 1
    return {"status": "original-body-probes-and-exact-slope-points-verified", "body_cases": body_cases,
            "point_cases": point_cases, "baseline_sha256": hashlib.sha256(original_source.encode()).hexdigest(),
            "proposal_sha256": hashlib.sha256(repaired_source.encode()).hexdigest(),
            "scope": "host-compiled original C function; body return and state effects; centre return; alternating gravity/input",
            "limitation": "does not emulate the NES compiler, caller context or bank mapper; independent NES replay remains required"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("original", type=Path)
    parser.add_argument("repaired", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = run(args.original, args.repaired)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
