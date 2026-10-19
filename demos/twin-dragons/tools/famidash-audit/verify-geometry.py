# SPDX-License-Identifier: AGPL-3.0-or-later
"""Decode release ROM level streams and compare every tile with the source index."""

import argparse
import hashlib
import json
from pathlib import Path
import re


def debug_symbols(path):
    segments, symbols = {}, {}
    for line in path.read_text().splitlines():
        if not line.startswith(("seg\t", "sym\t")):
            continue
        values = dict(re.findall(r'(\w+)=("[^"]*"|[^,\t]+)', line))
        if line.startswith("seg\t") and "ooffs" in values:
            segments[int(values["id"])] = (int(values["start"], 0), int(values["ooffs"], 0))
        elif line.startswith("sym\t") and "seg" in values and "val" in values:
            symbols[values["name"].strip('"')] = (int(values["seg"]), int(values["val"], 0))
    return {name: segments[seg][1] + address - segments[seg][0]
            for name, (seg, address) in symbols.items() if seg in segments}


def decompress(rom, offset):
    buffer, cursor = [0] * 256, 0
    end = 16 + ((offset - 16) // 8192 + 1) * 8192
    while offset < end:
        count = rom[offset]
        offset += 1
        if count == 255:
            return
        if count == 128:
            values = [rom[offset]]
            offset += 1
        else:
            assert 1 <= count <= 113, "invalid LZ copy"
            start, literal = rom[offset:offset + 2]
            offset += 2
            values = [buffer[(start + i) & 255] for i in range(count)] + [literal]
        for value in values:
            buffer[cursor] = value
            cursor = (cursor + 1) & 255
            yield value
    raise AssertionError("LZ stream ran past its ROM bank")


def compare(index, roms, builds):
    result = {"schema": 1, "status": "verified", "levels": [], "collision_tables": {}}
    header_bytes = index.get("level_header_bytes", 18)
    assert header_bytes in (14, 18), "unsupported level header layout"
    machines = {}
    for layout in index["levels"]:
        side = layout["side"]
        if side not in machines:
            rom = (roms / layout["rom"]).read_bytes()
            assert hashlib.sha256(rom).hexdigest() == layout["rom_sha256"], "ROM identity mismatch"
            debug = builds / ("main" if side == "main" else f"{side}-sides") / "famidash.dbg"
            machines[side] = rom, debug_symbols(debug)
            if "metatiles" in index:
                definitions = {}
                for line in debug.read_text().splitlines():
                    if not line.startswith("sym\t"):
                        continue
                    name = re.search(r'name="(COL_[^"]+)"', line)
                    value = re.search(r"(?:^|,)val=0x([\da-f]+)", line, re.I)
                    if name and value:
                        definitions[name[1]] = int(value[1], 16)
                expected = bytes(definitions[tile["collision"]] for tile in index["metatiles"])
                table = machines[side][1]["metatiles_coll"]
                actual = rom[table:table + len(expected)]
                assert len(expected) == 256 and actual == expected, "ROM collision classes differ from source index"
                result["collision_tables"][side] = {"entries": len(expected), "sha256": hashlib.sha256(actual).hexdigest()}
                if any("base_mask" in tile for tile in index["metatiles"]):
                    masks = [tile.get("base_mask", 0) for tile in index["metatiles"]]
                    assert all(isinstance(mask, int) and 0 <= mask <= 15 for mask in masks)
                    expected_bases = bytes(masks[i] | (masks[i + 1] << 4) for i in range(0, 256, 2))
                    start = machines[side][1]["_metatiles_bases"]
                    actual_bases = rom[start:start + 128]
                    assert actual_bases == expected_bases, "ROM spike-base metadata differs from source index"
                    result["collision_tables"][side]["base_metadata_sha256"] = hashlib.sha256(actual_bases).hexdigest()
        rom, symbols = machines[side]
        assert rom[:4] == b"NES\x1a", "expected an iNES ROM"
        prg_units = rom[4]
        if rom[7] & 12 == 8:
            assert rom[9] & 15 != 15, "exponent-encoded PRG sizes are unsupported"
            prg_units |= (rom[9] & 15) << 8
        prg_banks = prg_units * 2
        assert 16 + prg_banks * 8192 <= len(rom), "truncated PRG ROM"

        def pointer(prefix, number):
            low = rom[symbols[prefix + "_lo"] + number]
            high = rom[symbols[prefix + "_hi"] + number]
            bank = rom[symbols[prefix + "_bank"] + number]
            address = low + high * 256
            assert 0xa000 <= address < 0xc000 and bank < prg_banks, "invalid level pointer"
            return 16 + bank * 8192 + address - 0xa000

        header = pointer("_level_list", layout["level"])
        named_header = symbols.get("level_data_" + layout["name"])
        assert named_header is None or header == named_header, "level index selects a different named level"
        assert rom[header + header_bytes - 1] == layout["height"], "ROM level height differs"
        stream = decompress(rom, header + header_bytes)
        tiles, continuations = [], []
        wanted = layout["width"] * layout["height"]
        while len(tiles) < wanted:
            count = next(stream)
            if count >= 128:
                tiles.append(count & 127)
            else:
                value = next(stream)
                if count == value == 127:
                    chunk = next(stream)
                    assert chunk not in continuations, "cyclic continuation"
                    continuations.append(chunk)
                    assert chunk < symbols["_level_chunk_list_hi"] - symbols["_level_chunk_list_lo"]
                    stream = decompress(rom, pointer("_level_chunk_list", chunk))
                else:
                    tiles.extend([value] * (count + 1))
        tiles = tiles[:wanted]
        expected = [layout["tiles"][row * layout["width"] + column]
                    for column in range(layout["width"]) for row in range(layout["height"])]
        differences = [i for i, pair in enumerate(zip(tiles, expected)) if pair[0] != pair[1]]
        entry = {"side": side, "level": layout["level"], "name": layout["name"],
                 "tile_count": wanted, "continuations": continuations, "differences": len(differences),
                 "examples": [{"column": i // layout["height"], "row": i % layout["height"],
                               "source": expected[i], "rom": tiles[i]} for i in differences[:8]]}
        result["levels"].append(entry)
        if differences:
            result["status"] = "geometry-mismatch"
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("index", "roms", "builds", "output"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    result = compare(json.loads(args.index.read_text()), args.roms, args.builds)
    result["index_sha256"] = hashlib.sha256(args.index.read_bytes()).hexdigest()
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"status": result["status"], "levels": len(result["levels"]),
                      "mismatching_levels": sum(bool(x["differences"]) for x in result["levels"])}))
    raise SystemExit(result["status"] != "verified")
