# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check verified probe history against explicitly selected source regions."""

import argparse
import hashlib
import json
from pathlib import Path

from components import components


def review(index, history, side, level, seeds, linear_y, aligned_wave, regions_index=None):
    layout = next(item for item in index["levels"] if item["side"] == side and item["level"] == level)
    width, height = layout["width"], layout["height"]
    regions_index = index if regions_index is None else regions_index
    region_layout = next(item for item in regions_index["levels"] if item["side"] == side and item["level"] == level)
    assert all(layout[key] == region_layout[key] for key in ("width", "height", "top", "name")), "region coordinate layout differs"
    labels, groups = components(region_layout, regions_index["metatiles"])
    targets = set()
    for x, y in seeds:
        assert 0 <= x < width and 0 <= y < height
        number = labels[y * width + x]
        assert number >= 0, "region seed must belong to a fill component"
        targets.add(number)
    assert len(history["rows"]) == history["verified_frames"]
    hits, mismatches, samples = [], [], 0
    for frame, row in enumerate(history["rows"], 1):
        assert row["frame"] == frame, "complete consecutive history required"
        observation, values = row["observation"], row["values"]
        assert not observation["dual"] or "players" in row, "dual history requires both world positions"
        players = row.get("players", [{"player": 0, "x": observation["x"]}])
        assert {position["player"] for position in players} == ({0, 1} if observation["dual"] else {0})
        for position in players:
            player = position["player"]
            high = values["pit_previous_room"][player]
            if high == 255:
                continue
            y = values["pit_previous_y"][player] + high * (256 if linear_y else 240)
            x = position["x"] + ((values["pit_previous_x"][player] - (position["x"] & 255) + 128) % 256) - 128
            column, source_row = x // 16, (y - layout["top"]) // 16
            if not (0 <= column < width and 0 <= source_row < height):
                continue
            source = layout["tiles"][source_row * width + column]
            recorded = values["pit_previous_metatile"][player]
            if source != recorded:
                mismatches.append({"frame": frame, "player": player, "column": column, "row": source_row,
                                   "source": source, "recorded": recorded})
            offsets = [0]
            if aligned_wave and values["pit_previous_mode"][player] in (6, 10) and not values["pit_previous_mini"][player]:
                offsets.append(4)
            for offset in offsets:
                checked_column = (x + offset) // 16
                if not 0 <= checked_column < width:
                    continue
                samples += 1
                number = labels[source_row * width + checked_column]
                if number in targets:
                    hits.append({"frame": frame, "player": player, "column": checked_column, "row": source_row,
                                 "offset": offset, "component": number})
    assert samples, "no in-source history samples"
    return {"schema": 1, "status": "source-history-mismatch" if mismatches else "visits-regions" if hits else "avoids-regions",
            "verified_frames": history["verified_frames"], "sampled_points": samples,
            "linear_y": linear_y, "aligned_wave": aligned_wave,
            "components": {number: {key: groups[number][key] for key in ("bbox", "area", "sprites")}
                           for number in targets}, "source_history_mismatches": mismatches, "hits": hits,
            "limitation": "Engine probe history and selected source regions; this neither classifies region intent nor proves every course boundary correct."}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("index", "history", "output"):
        parser.add_argument(name, type=Path)
    parser.add_argument("--side", required=True)
    parser.add_argument("--level", required=True, type=int)
    parser.add_argument("--seed", required=True, action="append", nargs=2, type=int, metavar=("COLUMN", "ROW"))
    parser.add_argument("--linear-y", action="store_true")
    parser.add_argument("--aligned-wave", action="store_true")
    parser.add_argument("--regions-index", type=Path, help="preserve reviewed region footprints across boundary edits")
    args = parser.parse_args()
    index_bytes, history_bytes = args.index.read_bytes(), args.history.read_bytes()
    regions_bytes = args.regions_index.read_bytes() if args.regions_index else index_bytes
    result = review(json.loads(index_bytes), json.loads(history_bytes), args.side, args.level,
                    args.seed, args.linear_y, args.aligned_wave, json.loads(regions_bytes))
    result.update(index_sha256=hashlib.sha256(index_bytes).hexdigest(), history_sha256=hashlib.sha256(history_bytes).hexdigest())
    result["regions_index_sha256"] = hashlib.sha256(regions_bytes).hexdigest()
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"status": result["status"], "hits": len(result["hits"]),
                      "source_history_mismatches": len(result["source_history_mismatches"])}))
    raise SystemExit(bool(result["source_history_mismatches"]))
