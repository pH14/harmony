# SPDX-License-Identifier: AGPL-3.0-or-later
"""Group bank-aware collision reads by source region and observed survival."""

import argparse
import gzip
import hashlib
import json
from pathlib import Path

from components import components


def review(index, witness, cpu, regions_index=None):
    assert cpu["schema"] == 2, "bank-aware caller classification is required"
    assert cpu["rom_sha256"] == witness["rom_sha256"]
    layout = next(level for level in index["levels"]
                  if level["side"] == witness["options"]["side"]
                  and level["level"] == witness["boot_level"])
    assert layout["rom_sha256"] == witness["rom_sha256"], "source index targets another ROM"
    region_layout = layout
    if regions_index is not None:
        region_layout = next(level for level in regions_index["levels"]
                             if (level["side"], level["level"]) == (layout["side"], layout["level"]))
        assert all(region_layout[key] == layout[key] for key in ("name", "width", "height", "top")), (
            "region reference uses different level coordinates")
    labels, groups = components(region_layout, (regions_index or index)["metatiles"])
    states = {state["frame"]: state for state in cpu["states"]}
    for frame, state in states.items():
        expected = witness["trace"][frame - 1]
        if frame >= cpu["comparison_start_frame"]:
            assert {key: state[key] for key in expected} == expected, frame
    result = {"schema": 1, "status": "raw-probe-triage", "source_review_complete": False,
              "side": layout["side"], "level": layout["level"], "name": layout["name"],
              "components": {}, "source_mismatches": [], "outside_source": [],
              "centre_probes": 0,
              "region_geometry": "reference index" if regions_index is not None else "actual source index",
              "limitation": "Raw collision reads and end-of-frame survival only. Mode-specific geometry, "
                            "source intent, objectives and original-ROM reproduction remain required."}
    for sample in cpu["samples"]:
        if not sample["centre_probe"]:
            continue
        result["centre_probes"] += 1
        x = sample["sample_world_x"] // 16
        y = (sample["sample_world_y"] - layout["top"]) // 16
        event = {key: sample[key] for key in ("frame", "player", "mode", "mini", "metatile",
                                              "sample_world_x", "sample_world_y")}
        event.update(column=x, row=y)
        if not (0 <= x < layout["width"] and 0 <= y < layout["height"]):
            result["outside_source"].append(event)
            continue
        offset = y * layout["width"] + x
        if layout["tiles"][offset] != sample["metatile"]:
            result["source_mismatches"].append({**event, "source_metatile": layout["tiles"][offset]})
            continue
        number = labels[offset]
        if number < 0:
            continue
        group = result["components"].setdefault(str(number), {
            "geometry": groups[number], "live_entries": [], "death_frame_entries": [],
            "unsupported_state_entries": []})
        state = states[sample["frame"]]
        if state["dead"]:
            category = "death_frame_entries"
        elif state["state"] != 2 or any(state[key] for key in ("debug", "paused", "invincible")):
            category = "unsupported_state_entries"
        else:
            category = "live_entries"
        group[category].append(event)
    if result["source_mismatches"]:
        result["status"] = "source-mismatch-requires-review"
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("index", "case", "cpu", "output"):
        parser.add_argument(name, type=Path)
    parser.add_argument("--regions-index", type=Path,
                        help="preserve reviewed footprints after source edits split or replace a region")
    args = parser.parse_args()
    def read(path):
        raw = path.read_bytes()
        return raw, json.loads(gzip.decompress(raw) if path.suffix == ".gz" else raw)
    index_bytes, index = read(args.index)
    case_bytes, witness = read(args.case)
    cpu_bytes, cpu = read(args.cpu)
    assert cpu["witness_sha256"] == hashlib.sha256(case_bytes).hexdigest(), "CPU trace binds another case"
    region_bytes, regions = read(args.regions_index) if args.regions_index else (None, None)
    result = review(index, witness, cpu, regions)
    result["inputs"] = {"index_sha256": hashlib.sha256(index_bytes).hexdigest(),
                        "case_sha256": hashlib.sha256(case_bytes).hexdigest(),
                        "cpu_sha256": hashlib.sha256(cpu_bytes).hexdigest(),
                        "rom_sha256": witness["rom_sha256"], "core_sha256": cpu["core_sha256"],
                        "debug_sha256": cpu["debug_sha256"]}
    if region_bytes is not None:
        result["inputs"]["regions_index_sha256"] = hashlib.sha256(region_bytes).hexdigest()
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"status": result["status"], "components": len(result["components"]),
                      "centre_probes": result["centre_probes"],
                      "source_mismatches": len(result["source_mismatches"])}))
    if result["source_mismatches"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
