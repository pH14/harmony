# SPDX-License-Identifier: AGPL-3.0-or-later
"""Export collision geometry from the pinned Famidash source without JSON5 dependencies."""

import argparse
import bisect
import hashlib
import json
import re
import subprocess
import xml.etree.ElementTree as ET
from pathlib import Path


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read_level(path):
    root = ET.parse(path).getroot()
    assert root.get("orientation") == "orthogonal" and root.get("infinite") == "0"
    assert root.get("tilewidth") == root.get("tileheight") == "16"
    layer = min(
        (layer for layer in root.findall("layer") if layer.get("name", "") in ("", "BG")),
        key=lambda layer: int(layer.get("id")),
    )
    data = layer.find("data")
    assert data.get("encoding") == "csv" and data.get("compression") is None
    width, height = int(layer.get("width")), int(layer.get("height"))
    assert 0 < height <= 57
    gids = [int(value) for value in data.text.replace("\n", "").split(",") if value.strip()]
    firsts = sorted(int(tileset.get("firstgid")) for tileset in root.findall("tileset"))
    tiles = [gid - firsts[bisect.bisect_right(firsts, gid) - 1] if gid else 0 for gid in gids]
    assert len(tiles) == width * height and all(0 <= tile < 256 for tile in tiles)
    sprites = []
    for objects in root.findall("layer"):
        if objects.get("name") != "SP":
            continue
        assert int(objects.get("width")) == width and int(objects.get("height")) == height
        data = objects.find("data")
        assert data.get("encoding") == "csv" and data.get("compression") is None
        gids = [int(value) for value in data.text.replace("\n", "").split(",") if value.strip()]
        assert len(gids) == width * height
        for cell, gid in enumerate(gids):
            if gid:
                sprite = gid - firsts[bisect.bisect_right(firsts, gid) - 1]
                assert 0 <= sprite < 256
                sprites.append({"column": cell % width, "row": cell // width, "id": sprite})
    return {"width": width, "height": height, "top": (57 - height) * 16, "tiles": tiles, "sprites": sprites}


def export(source):
    version_path = Path(__file__).resolve().parents[4] / "workloads/nes/famidash-versions.env"
    versions = dict(re.findall(r"^(\w+)=(.*)$", version_path.read_text(), re.M))
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True).strip()
    assert revision == versions["FAMIDASH_COMMIT"], "Use the pinned source revision"
    dirty = subprocess.run(["git", "diff", "--quiet", "HEAD"], cwd=source).returncode
    assert dirty == 0, "Export original geometry from a clean source checkout"
    definitions = re.findall(
        r'^Metatile\s+"([^"]+)"[^\n]*,\s*(COL_\w+)\s*$',
        re.sub(r";[^\n]*", "", (source / "METATILES/metatiles.inc").read_text()), re.M,
    )
    assert len(definitions) == 256
    levels = []
    for side, letter, rom, sha_key in (
        ("main", "A", "famidash.nes", "FAMIDASH_UPSTREAM_ROM_SHA256"),
        ("b", "B", "famidash-b-sides.nes", "FAMIDASH_B_SIDES_UPSTREAM_ROM_SHA256"),
        ("c", "C", "famidash-c-sides.nes", "FAMIDASH_C_SIDES_UPSTREAM_ROM_SHA256"),
        ("d", "D", "famidash-d-sides.nes", "FAMIDASH_D_SIDES_UPSTREAM_ROM_SHA256"),
    ):
        table = source / f"LEVELS/include/lvlset_{letter}/all_level_table.s"
        object_names = {int(value, 16): name for name, value in re.findall(
            r"^#define\s+(\w+)\s+(0x[0-9a-fA-F]+)\s*$",
            (table.parent / "objdefines.h").read_text(), re.M)}
        section = table.read_text().split("_level_list_lo:", 1)[1].split("_level_list_hi:", 1)[0]
        names = re.findall(r"\.lobyte\(level_data_(\w+)\)", section)
        paths = {path.stem.lower(): path for path in (source / f"LEVELS/LEVEL DATA/lvlset_{letter}").glob("*.tmx")}
        for level, name in enumerate(names):
            if name.lower() not in paths:
                name = re.sub(r"_\d+$", "", name)
            path = paths[name.lower()]
            geometry = read_level(path)
            for sprite in geometry["sprites"]:
                sprite["name"] = object_names.get(sprite["id"], "UNKNOWN")
            levels.append({
                "side": side, "level": level, "name": name,
                "source": path.relative_to(source).as_posix(),
                "source_sha256": digest(path.read_bytes()),
                "rom": rom, "rom_sha256": versions[sha_key], **geometry,
            })
    assert len(levels) == 109
    return {"schema": 1, "source_revision": revision,
            "metatiles": [{"name": name, "collision": collision} for name, collision in definitions],
            "levels": levels}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = export(args.source)
    args.output.write_text(json.dumps(result, separators=(",", ":")) + "\n")
    print(f"Exported {len(result['levels'])} levels and {len(result['metatiles'])} metatiles")
