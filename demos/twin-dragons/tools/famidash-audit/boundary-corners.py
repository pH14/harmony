# SPDX-License-Identifier: AGPL-3.0-or-later
"""Inventory spike stairs, rounded hazards and quarter blocks beside fill."""

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
        corners, rounded_caps, quarter_caps, cap_components = [], [], [], {}
        for y in range(height):
            for x in range(width):
                offset = y * width + x
                if tiles[offset] not in interiors:
                    continue
                group = groups[labels[offset]]
                for dx, dy, mask, edge in [(0, -1, 2, "bottom"), (0, 1, 1, "top"),
                                            (-1, 0, 8, "right"), (1, 0, 4, "left")]:
                    nx, ny = x + dx, y + dy
                    if not (0 <= nx < width and 0 <= ny < height):
                        continue
                    hazard = metatiles[tiles[ny * width + nx]]
                    if hazard["collision"] in {"COL_UP_LEFT", "COL_UP_RIGHT", "COL_DOWN_LEFT", "COL_DOWN_RIGHT"}:
                        cap_components[group["number"]] = {key: group[key] for key in
                            ("bbox", "area", "boundary", "non_none_boundary_fraction", "sprites")}
                        quarter_caps.append({"column": x, "row": y, "block_column": nx, "block_row": ny,
                                             "block": hazard["name"], "block_collision": hazard["collision"],
                                             "unsealed_edge": edge, "component": group["number"],
                                             "disposition": "needs-input-and-intent-review"})
                    if ("SAW" not in hazard["name"] or hazard["collision"] == "COL_NONE"
                            or hazard.get("base_mask", 0) & mask):
                        continue
                    cap_components[group["number"]] = {key: group[key] for key in
                        ("bbox", "area", "boundary", "non_none_boundary_fraction", "sprites")}
                    rounded_caps.append({"column": x, "row": y, "hazard_column": nx, "hazard_row": ny,
                                         "hazard": hazard["name"], "hazard_collision": hazard["collision"],
                                         "unsealed_edge": edge, "component": group["number"],
                                         "disposition": "needs-input-and-intent-review"})
                for dx, dy, mask, edge in [(0, -1, 2, "bottom"), (0, 1, 1, "top"),
                                           (-1, 0, 8, "right"), (1, 0, 4, "left")]:
                    if not (0 <= x + dx < width and 0 <= y + dy < height):
                        continue
                    normal = dy * width + dx
                    if not metatiles[tiles[offset + normal]].get("base_mask", 0) & mask:
                        continue
                    for tx, ty in ([(-1, 0), (1, 0)] if dy else [(0, -1), (0, 1)]):
                        if not (0 <= x + tx < width and 0 <= y + ty < height
                                and 0 <= x + dx + tx < width and 0 <= y + dy + ty < height):
                            continue
                        tangent = ty * width + tx
                        if not metatiles[tiles[offset + tangent]].get("base_mask", 0) & mask:
                            continue
                        number = labels[offset]
                        group = groups[number]
                        diagonal = metatiles[tiles[offset + normal + tangent]]
                        corners.append({"column": x, "row": y, "metatile": tiles[offset],
                                        "base_edge": edge, "normal_offset": normal,
                                        "tangent_offset": tangent, "diagonal_tile": diagonal["name"],
                                        "diagonal_collision": diagonal["collision"],
                                        "component": number, "component_bbox": group["bbox"],
                                        "component_area": group["area"],
                                        "component_sprites": group["sprites"],
                                        "disposition": "needs-input-and-intent-review"})
        if corners or rounded_caps or quarter_caps:
            result.append({"side": layout["side"], "level": layout["level"], "name": layout["name"],
                           "source": layout["source"], "source_sha256": layout["source_sha256"],
                           "corners": corners, "rounded_caps": rounded_caps, "quarter_caps": quarter_caps,
                           "cap_components": cap_components})
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("index", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    raw = args.index.read_bytes()
    levels = inventory(json.loads(raw))
    result = {"schema": 4, "index_sha256": hashlib.sha256(raw).hexdigest(),
              "purpose": "triage only; topology is not proof of a bug or permission to block a route",
              "levels": levels, "corner_count": sum(len(level["corners"]) for level in levels),
              "rounded_cap_count": sum(len(level["rounded_caps"]) for level in levels),
              "quarter_cap_count": sum(len(level["quarter_caps"]) for level in levels)}
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"levels": len(levels), "corners": result["corner_count"],
                      "rounded_caps": result["rounded_cap_count"], "quarter_caps": result["quarter_cap_count"]}))
