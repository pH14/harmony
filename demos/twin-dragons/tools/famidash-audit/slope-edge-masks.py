# SPDX-License-Identifier: AGPL-3.0-or-later
"""Measure slope edge coverage by host-compiling the original collision C."""

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import tempfile


spec = importlib.util.spec_from_file_location("slope_model", Path(__file__).with_name("check-slope-context.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    header = (args.source / "METATILES/metatiles.h").read_bytes()
    definitions = dict((name, int(value, 0)) for name, value in re.findall(
        r"^#define (COL_SLOPE_\w+)\s+(0x[\da-fA-F]+|\d+)", header.decode(), re.M))
    mode = re.search(r"^#define GAMEMODE_CUBE\s+(0x[\da-fA-F]+|\d+)",
                     (args.source / "SAUCE/famidash.h").read_text(), re.M)
    assert mode and definitions
    masks = {}
    with tempfile.TemporaryDirectory(prefix="famidash-slope-edges-") as temporary:
        core, source = module.compile_model(args.source, Path(temporary), "edges")
        for name, kind in definitions.items():
            rows = [sum((core.evaluate(kind, x, y, int(mode[1], 0), 0, 0) & 255) << x
                        for x in range(16)) for y in range(16)]
            edges = {"top": rows[0], "bottom": rows[15],
                     "left": sum(((row >> 0) & 1) << y for y, row in enumerate(rows)),
                     "right": sum(((row >> 15) & 1) << y for y, row in enumerate(rows))}
            masks[name] = {"collision_value": kind, "rows": rows,
                           "edges": {edge: {"mask": mask, "solid_pixels": mask.bit_count()}
                                     for edge, mask in edges.items()}}
    result = {"schema": 1, "source_revision": subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=args.source, text=True).strip(),
        "slope_function_sha256": hashlib.sha256(module.function(source).encode()).hexdigest(),
        "metatiles_header_sha256": hashlib.sha256(header).hexdigest(), "pixel_cases": len(definitions) * 256,
        "model": "host-compiled bg_coll_slope return in normal cube mode, before wave exemptions",
        "limitation": "Point coverage only; no body ejection, swept segments, renderer or NES compiler verification.",
        "slopes": masks}
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"slopes": len(masks), "pixel_cases": result["pixel_cases"]}))
