# SPDX-License-Identifier: AGPL-3.0-or-later
"""Inventory spike stair corners for source review, without proposing tile edits."""

import argparse
import hashlib
import json
from pathlib import Path

from components import components


def inventory(index):
    metatiles = index["metatiles"]
    interiors = {i for i, tile in enumerate(metatiles)
                 if tile["name"] in {"BLACK_NONE", "X_NO_OUTLINE", "CHECKERBOARD_15"}}
    result = []
    for layout in index["levels"]:
        width, height, tiles = layout["width"], layout["height"], layout["tiles"]
        labels, groups = components(layout, metatiles)
        corners = []
        for y in range(1, height - 1):
            for x in range(1, width - 1):
                offset = y * width + x
                if tiles[offset] not in interiors:
                    continue
                for normal, mask, edge in [(-width, 2, "bottom"), (width, 1, "top"),
                                           (-1, 8, "right"), (1, 4, "left")]:
                    if not metatiles[tiles[offset + normal]].get("base_mask", 0) & mask:
                        continue
                    for tangent in ([-1, 1] if abs(normal) == width else [-width, width]):
                        if not metatiles[tiles[offset + tangent]].get("base_mask", 0) & mask:
                            continue
                        number = labels[offset]
                        group = groups[number]
                        objectives = [sprite for sprite in layout["sprites"]
                                      if 0 <= sprite["column"] < width
                                      and 0 <= sprite["row"] < height
                                      and labels[sprite["row"] * width + sprite["column"]] == number]
                        diagonal = metatiles[tiles[offset + normal + tangent]]
                        corners.append({"column": x, "row": y, "metatile": tiles[offset],
                                        "base_edge": edge, "normal_offset": normal,
                                        "tangent_offset": tangent, "diagonal_tile": diagonal["name"],
                                        "diagonal_collision": diagonal["collision"],
                                        "component": number, "component_bbox": group["bbox"],
                                        "component_area": group["area"],
                                        "component_sprites": objectives,
                                        "disposition": "needs-input-and-intent-review"})
        if corners:
            result.append({"side": layout["side"], "level": layout["level"], "name": layout["name"],
                           "source": layout["source"], "source_sha256": layout["source_sha256"],
                           "corners": corners})
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("index", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    raw = args.index.read_bytes()
    levels = inventory(json.loads(raw))
    result = {"schema": 1, "index_sha256": hashlib.sha256(raw).hexdigest(),
              "purpose": "triage only; topology is not proof of a bug or permission to block a route",
              "levels": levels, "corner_count": sum(len(level["corners"]) for level in levels)}
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"levels": len(levels), "corners": result["corner_count"]}))
