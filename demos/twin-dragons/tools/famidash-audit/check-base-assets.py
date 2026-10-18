# SPDX-License-Identifier: AGPL-3.0-or-later
"""Verify declared spike bases against every background graphics variant."""

import argparse
import hashlib
import itertools
import json
from pathlib import Path
import re


def pixels(data, tiles):
    result = [[0] * 16 for _ in range(16)]
    for quadrant, tile in enumerate(tiles):
        for y in range(8):
            for x in range(8):
                result[quadrant // 2 * 8 + y][quadrant % 2 * 8 + x] = (
                    ((data[tile * 16 + y] >> (7 - x)) & 1)
                    | (((data[tile * 16 + y + 8] >> (7 - x)) & 1) << 1))
    return result


def run(source):
    rows = [line for line in (source / "METATILES/metatiles.inc").read_text().splitlines()
            if line.startswith("Metatile ")]
    edges = {"COL_PIT": "bottom", "COL_PIT_TOP": "top", "COL_FULL_FLOOR_SPIKE": "bottom",
             "COL_FULL_CEILING_SPIKE": "top", "COL_FULL_LEFT_SPIKE": "left",
             "COL_FULL_RIGHT_SPIKE": "right", "COL_LEFT_BASE_SPIKE": "left",
             "COL_RIGHT_BASE_SPIKE": "right"}
    metadata = source / "METATILES/spike_bases.inc"
    if metadata.exists():
        masks = dict(re.findall(r'SpikeBase\s+"([^"]+)",\s*(\d+)', metadata.read_text()))
        declared, seen = [], set()
        for number, line in enumerate(rows):
            name = re.search(r'"([^"]+)"', line)[1]
            if name not in masks:
                continue
            mask = int(masks[name])
            assert 0 < mask <= 15
            seen.add(name)
            for bit, edge in [(1, "top"), (2, "bottom"), (4, "left"), (8, "right")]:
                if mask & bit:
                    declared.append((number, line, edge))
        assert seen == masks.keys(), "metadata names an absent tile"
    else:
        declared = [(number, line, edges[match[1]]) for number, line in enumerate(rows)
                    if (match := re.search(r",\s*(COL_\w+)\s*$", line)) and match[1] in edges]
    assert declared, "source declares no spike bases"
    files, cases, checks = {}, 0, []
    for spikes, blocks, phase in itertools.product("ABC", "ABCD", range(2)):
        data = b""
        for name in (f"Spikes{spikes}", f"Blocks{blocks}", "slopesA", "SawbladesA"):
            path = source / "GRAPHICS/Level Tiles" / f"{name}.chr"
            raw = path.read_bytes()
            files[str(path.relative_to(source))] = hashlib.sha256(raw).hexdigest()
            offset = phase * 1024 if name.startswith(("Spikes", "Blocks", "Sawblades")) else 0
            data += raw[offset:offset + 1024]
        assert len(data) == 4096
        background = pixels(data, [0] * 4)
        for number, line, edge in declared:
            tile = pixels(data, [int(v, 16) for v in re.findall(r"\$([0-9a-fA-F]{2})", line)][:4])
            positions = [(x, 15 if edge == "bottom" else 0) for x in range(16)] if edge in ("top", "bottom") else [
                (0 if edge == "left" else 15, y) for y in range(16)]
            assert all(tile[y][x] != background[y][x] for x, y in positions), (number, edge, spikes, blocks, phase)
            cases += 1
    for number, line, edge in declared:
        checks.append({"metatile": number, "name": re.search(r'"([^"]+)"', line)[1], "edge": edge})
    return {"status": "declared-bases-cover-full-edge-in-every-graphics-variant", "cases": cases,
            "variants": 24, "metatiles": checks, "graphics_sha256": files}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=lambda value: Path(value).resolve())
    print(json.dumps(run(parser.parse_args().source)))
