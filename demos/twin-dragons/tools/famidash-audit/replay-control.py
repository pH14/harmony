# SPDX-License-Identifier: AGPL-3.0-or-later
"""Replay ordinary controller input on another build and retain actual outcomes."""

import argparse
import gzip
import hashlib
import json
from pathlib import Path
import re

from fceumm import Fceumm, SymbolFceumm


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("case", "core", "rom", "output"):
        parser.add_argument(name, type=Path)
    parser.add_argument("--driver", choices=("fceumm", "mesen"), default="fceumm")
    parser.add_argument("--symbols", type=Path)
    parser.add_argument("--level", type=int)
    parser.add_argument("--side")
    args = parser.parse_args()
    case_bytes = args.case.read_bytes()
    witness = json.loads(gzip.decompress(case_bytes))
    plain, symbolic = Fceumm, SymbolFceumm
    if args.driver == "mesen":
        from mesen import Mesen, SymbolMesen
        plain, symbolic = Mesen, SymbolMesen
    layout = witness.get("target_layout")
    if layout:
        assert args.symbols, "supply the target build's DBG file"
        symbols = {}
        for line in args.symbols.read_text().splitlines():
            if not line.startswith("sym\t"):
                continue
            name = re.search(r'name="([^" ]+)"', line)
            value = re.search(r'(?:^|,)val=0x([0-9a-f]+)', line, re.I)
            if name and value:
                symbols[name[1]] = int(value[1], 16)
        layout = {"kind": layout["kind"], "symbols": {
            name: symbols["_" + name] for name in layout["symbols"]}}
        core = symbolic(args.core, args.rom, layout)
    else:
        assert not args.symbols, "symbol decoding requires a symbol-based source witness"
        core = plain(args.core, args.rom)
    options = witness["options"]
    core.startup_frames = options.get("fceumm_startup_released_frames") or options.get("startup_released_frames") or 180
    side = args.side or options["side"]
    level = witness["boot_level"] if args.level is None else args.level
    core.boot(side, level, options.get("menu_delay", 0))
    trace = []
    for action in witness["actions"]:
        assert action["buttons"] in (0, 1) and 0 < action["frames"] <= 200000
        for _ in range(action["frames"]):
            core.step(action["buttons"])
            state = core.observe()
            assert not (state["debug"] or state["paused"])
            trace.append(state)
            if state["dead"] or state["state"] != 2:
                break
        if trace[-1]["dead"] or trace[-1]["state"] != 2:
            break
    differences = [i + 1 for i, state in enumerate(trace)
                   if i >= len(witness["trace"]) or
                   {key: value for key, value in state.items() if key != "level"} !=
                   {key: value for key, value in witness["trace"][i].items() if key != "level"}]
    result = {"engine": "nes-controller-replay", "discovery_engine": witness.get("discovery_engine", witness["engine"]),
              "purpose": "positive-control" if trace[-1]["state"] == 3 else "rejected-positive-control",
              "source_case": str(args.case), "case_sha256": hashlib.sha256(case_bytes).hexdigest(),
              "rom_sha256": hashlib.sha256(core.rom).hexdigest(),
              "core_sha256": hashlib.sha256(args.core.read_bytes()).hexdigest(),
              "options": {"side": side, "level": level, "startup_released_frames": core.startup_frames,
                          "menu_delay": options.get("menu_delay", 0)},
              "comparison": {"excluded_fields": ["level"], "difference_frames": differences,
                             "source_frames": len(witness["trace"]), "actual_frames": len(trace)},
              "actions": witness["actions"], "trace": trace}
    if args.symbols:
        result["debug_sha256"] = hashlib.sha256(args.symbols.read_bytes()).hexdigest()
    args.output.write_bytes(gzip.compress(json.dumps(result).encode()))
    print(json.dumps({key: value for key, value in result.items() if key not in ("trace", "actions", "comparison")} |
                     {"frames": len(trace), "endpoint": trace[-1], "difference_frames": len(differences)}))


if __name__ == "__main__":
    main()
