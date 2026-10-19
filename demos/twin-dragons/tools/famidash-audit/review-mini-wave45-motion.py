# SPDX-License-Identifier: AGPL-3.0-or-later
"""Find exact mini-wave45 crossings in bound, bank-aware ordinary-input traces."""

import argparse
from fractions import Fraction
import gzip
import hashlib
import json
from pathlib import Path


PLANES = {"COL_SLOPE_RD45": (-1, -1, -16), "COL_SLOPE_LD45": (1, -1, 0),
          "COL_SLOPE_RU45": (-1, 1, 0), "COL_SLOPE_LU45": (1, 1, 16)}


def intersection(x, y, dx, dy, kind):
    lower, upper = Fraction(0), Fraction(1)
    for a, b, limit in [(-1, 0, 0), (1, 0, 16), (0, -1, 0), (0, 1, 16), PLANES[kind]]:
        distance, velocity = a * x + b * y, a * dx + b * dy
        if not velocity:
            if distance > limit:
                return None
        elif velocity > 0:
            upper = min(upper, Fraction(limit - distance, velocity))
        else:
            lower = max(lower, Fraction(limit - distance, velocity))
    return (lower, upper) if lower <= upper else None


def review(index, case, cpu):
    assert cpu["schema"] == 2, "bank-aware caller classification is required"
    assert cpu["rom_sha256"] == case["rom_sha256"]
    layout = next(level for level in index["levels"]
                  if (level["side"], level["level"]) == (case["options"]["side"], case["boot_level"]))
    assert layout["rom_sha256"] == case["rom_sha256"]
    states = {s["frame"]: s for s in cpu["states"]}
    for frame, state in states.items():
        expected = case["trace"][frame - 1]
        if frame >= cpu["comparison_start_frame"]:
            assert {key: state[key] for key in expected} == expected, frame
    previous, hits, mismatches = {}, [], []
    motions = 0
    for sample in cpu["samples"]:
        if not sample["centre_probe"]:
            continue
        col = sample["sample_world_x"] // 16
        row = (sample["sample_world_y"] - layout["top"]) // 16
        if 0 <= col < layout["width"] and 0 <= row < layout["height"]:
            tile = layout["tiles"][row * layout["width"] + col]
            if tile != sample["metatile"]:
                mismatches.append({"frame": sample["frame"], "column": col, "row": row,
                                   "actual": sample["metatile"], "source": tile})
        old = previous.get(sample["player"])
        previous[sample["player"]] = sample
        if old is None or sample["frame"] != old["frame"] + 1:
            continue
        if any(s["mode"] != 6 or not s["mini"] for s in (old, sample)):
            continue
        dx, dy = (sample["sample_world_" + axis] - old["sample_world_" + axis] for axis in ("x", "y"))
        if abs(dx) > 16 or abs(dy) > 16 or not (dx or dy):
            continue
        motions += 1
        columns = {old["sample_world_x"] // 16, col}
        rows = {(old["sample_world_y"] - layout["top"]) // 16, row}
        for column in columns:
            for tile_row in rows:
                if not (0 <= column < layout["width"] and 0 <= tile_row < layout["height"]):
                    continue
                tile = layout["tiles"][tile_row * layout["width"] + column]
                kind = index["metatiles"][tile]["collision"]
                if kind not in PLANES:
                    continue
                x = Fraction(2 * old["sample_world_x"] + 1, 2) - column * 16
                y = Fraction(2 * old["sample_world_y"] + 1, 2) - layout["top"] - tile_row * 16
                interval = intersection(x, y, dx, dy, kind)
                if interval is None:
                    continue
                state = states[sample["frame"]]
                category = ("death-frame" if state["dead"] else "unsupported-state"
                            if state["state"] != 2 or any(state[k] for k in ("debug", "paused", "invincible"))
                            else "live")
                hits.append({"frame": sample["frame"], "player": sample["player"],
                             "column": column, "row": tile_row, "metatile": tile, "collision": kind,
                             "old_point": [old["sample_world_x"], old["sample_world_y"]],
                             "new_point": [sample["sample_world_x"], sample["sample_world_y"]],
                             "parameter_interval": list(map(str, interval)),
                             "positive_interval": interval[0] < interval[1], "category": category})
    return {"schema": 1, "status": "source-mismatch-requires-review" if mismatches else "motion-triage",
            "motions": motions, "intersections": hits, "source_mismatches": mismatches,
            "coordinate_convention": "Raw probe pixels represented by their centres (+1/2 on both axes).",
            "limitation": "Geometric leads, not intent verdicts. Mode/size changes and motions over 16 pixels are excluded; teleport, reset and history validity require separate review."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("index", "case", "cpu", "output"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    def read(path):
        raw = path.read_bytes()
        return raw, json.loads(gzip.decompress(raw) if path.suffix == ".gz" else raw)
    index_bytes, index = read(args.index)
    case_bytes, case = read(args.case)
    cpu_bytes, cpu = read(args.cpu)
    assert cpu["witness_sha256"] == hashlib.sha256(case_bytes).hexdigest()
    result = review(index, case, cpu)
    result["inputs"] = {"index_sha256": hashlib.sha256(index_bytes).hexdigest(),
                        "case_sha256": hashlib.sha256(case_bytes).hexdigest(),
                        "cpu_sha256": hashlib.sha256(cpu_bytes).hexdigest(),
                        "rom_sha256": case["rom_sha256"], "core_sha256": cpu["core_sha256"],
                        "debug_sha256": cpu["debug_sha256"]}
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"motions": result["motions"], "intersections": len(result["intersections"]),
                      "live": sum(h["category"] == "live" for h in result["intersections"]),
                      "source_mismatches": len(result["source_mismatches"])}))
    if result["source_mismatches"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
