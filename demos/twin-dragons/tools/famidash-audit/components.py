# SPDX-License-Identifier: AGPL-3.0-or-later
"""Group replayed entry signals by source geometry without declaring them bugs."""

import argparse
import json
from collections import Counter, deque
from pathlib import Path


def components(layout, metatiles):
    width, height, tiles = layout["width"], layout["height"], layout["tiles"]
    labels = [-1] * len(tiles)
    result = []
    interiors = {i for i, tile in enumerate(metatiles)
                 if tile["name"] in {"CHECKERBOARD_15", "X_NO_OUTLINE", "BLACK_NONE"}}
    for offset, tile in enumerate(tiles):
        if tile not in interiors or labels[offset] >= 0:
            continue
        number = len(result)
        labels[offset] = number
        queue = deque([offset])
        members, boundary, opens = [], Counter(), []
        while queue:
            cell = queue.popleft()
            x, y = cell % width, cell // width
            members.append((x, y))
            for dx, dy, direction in [(-1, 0, "left"), (1, 0, "right"), (0, -1, "up"), (0, 1, "down")]:
                nx, ny = x + dx, y + dy
                if not (0 <= nx < width and 0 <= ny < height):
                    boundary["source-edge"] += 1
                    continue
                neighbor = ny * width + nx
                if tiles[neighbor] == tile:
                    if labels[neighbor] < 0:
                        labels[neighbor] = number
                        queue.append(neighbor)
                else:
                    collision = metatiles[tiles[neighbor]]["collision"]
                    boundary[collision] += 1
                    if collision == "COL_NONE":
                        opens.append({"column": x, "row": y, "face": direction, "neighbor": tiles[neighbor]})
        xs, ys = zip(*members)
        faces = sum(value for kind, value in boundary.items() if kind != "source-edge")
        blocking = faces - boundary["COL_NONE"]
        result.append({"number": number, "tile": tile, "name": metatiles[tile]["name"], "area": len(members),
                       "bbox": [min(xs), min(ys), max(xs), max(ys)], "boundary": dict(boundary),
                       "non_none_boundary_fraction": blocking / max(1, faces), "open_faces": opens, "entries": []})
    return labels, result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("index", type=Path)
    parser.add_argument("review", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    index = json.loads(args.index.read_text())
    review = json.loads(args.review.read_text())
    output = {"schema": 1, "components": {}, "other_entries": {}}
    by_level = {}
    for layout in index["levels"]:
        key = f"{layout['side']}:{layout['level']}"
        entries = [(name, entry) for name, entry in review["entries"].items() if name.startswith(key + ":")]
        if not entries:
            continue
        labels, groups = components(layout, index["metatiles"])
        for name, entry in entries:
            tile = entry["representative"].get("tile")
            if ":fill:" not in name or not tile:
                output["other_entries"][name] = entry
                continue
            number = labels[tile["row"] * layout["width"] + tile["column"]]
            if number < 0:
                output["other_entries"][name] = entry
                continue
            group = groups[number]
            group["entries"].append({"key": name, "max_duration": entry["max_duration"],
                                      "max_alive_after": entry["max_alive_after"], "representative": entry["representative"]})
        for group in groups:
            if group["entries"]:
                output["components"][f"{key}:{group['number']}"] = group
    args.output.write_text(json.dumps(output, indent=2) + "\n")
    print(len(output["components"]), "source components;", len(output["other_entries"]), "other signals")
