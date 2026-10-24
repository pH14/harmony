#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Read-only stall diagnostics; never feed byte rankings back into search."""
import argparse
import collections
import hashlib
import json
import math
from pathlib import Path
import subprocess
import sys
import tempfile


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def fields(value, prefix=""):
    if isinstance(value, dict):
        for key, child in value.items():
            yield from fields(child, f"{prefix}.{key}" if prefix else key)
    elif not isinstance(value, (list, dict)):
        yield prefix, value


def ram(snapshot):
    """Map inspector-decoded RAM to CPU addresses without restoring a target."""
    decoded = snapshot.get("decoded_ram")
    if decoded is not None:
        work, save = bytes(decoded["work"]), bytes(decoded.get("save", []))
        if len(work) != 2048 or len(save) > 8192:
            raise ValueError("invalid decoded RAM lengths")
        values = {f"${i:04X}": byte for i, byte in enumerate(work)}
        values.update({f"${i + 0x6000:04X}": byte for i, byte in enumerate(save)})
        return values, None
    return {}, "snapshot has no decoded native QuickNES RAM"


def distribution(values, total):
    counts = collections.Counter(canonical(v) for v in values)
    n = sum(counts.values())
    entropy = -sum((c / n) * math.log2(c / n) for c in counts.values()) if n else 0.0
    return {"distinct": len(counts), "samples": n, "missing": total - n,
            "entropy_bits": entropy,
            "values": [{"value": json.loads(v), "count": c} for v, c in sorted(counts.items())]}


def variation(entries, requested):
    snapshots = {}
    missing = 0
    for entry in entries:
        snapshot = entry.get("snapshot")
        if snapshot is None:
            missing += 1
        else:
            snapshots[hashlib.sha256(canonical(snapshot).encode()).hexdigest()] = snapshot
    byte_values, observation_values = collections.defaultdict(list), collections.defaultdict(list)
    unsupported = collections.Counter()
    for snapshot in snapshots.values():
        values, reason = ram(snapshot)
        if reason:
            unsupported[reason] += 1
        for name, value in values.items():
            byte_values[name].append(value)
        for name, value in fields(snapshot.get("observation", {})):
            observation_values[name].append(value)
    def rank(source):
        rows = [{"field": name, **distribution(values, len(snapshots))} for name, values in source.items()]
        varying = [row for row in rows if row["distinct"] > 1]
        varying.sort(key=lambda row: (-row["entropy_bits"], -row["distinct"], row["field"]))
        for index, row in enumerate(varying):
            row["rank"] = index + 1
        return rows, varying
    all_bytes, varying = rank(byte_values)
    _, observations = rank(observation_values)
    by_name = {row["field"]: row for row in all_bytes}
    diagnostics = []
    for name, label in requested:
        row = by_name.get(name)
        diagnostics.append({"field": name, "status": "unavailable"} if row is None else
                           {**row, "status": "varying" if row["distinct"] > 1 else
                            "constant_in_available_states" if missing or row["missing"] else "constant"})
        if label:
            diagnostics[-1]["name"] = label
    return {"retained_entries": len(entries), "distinct_snapshot_states": len(snapshots),
            "missing_snapshots": missing, "unsupported_ram_snapshots": dict(unsupported),
            "ranking": "descending Shannon entropy across distinct retained snapshots; distinct count then field name break ties; descriptive, not causal",
            "top_ten_ram": varying[:10], "varying_ram": varying,
            "top_ten_observations": observations[:10], "varying_observations": observations, "requested_bytes": diagnostics}


def read_progress(path):
    previous = -1
    found = False
    with open(path) as source:
        for line in source:
            value = json.loads(line)
            sequence = value["executions"]
            if type(sequence) is not int or sequence < previous:
                raise ValueError("progress execution counts are not monotonic")
            previous, found = sequence, True
            yield value
    if not found:
        raise ValueError("empty progress log")


def report(metadata, entries, progress, requested):
    grouped = collections.defaultdict(list)
    cell_tiers = {}
    cell_counts = collections.defaultdict(lambda: [0, 0])
    for entry in entries:
        cell_tiers[entry["cell"]] = entry["tier"]
        cell_counts[entry["cell"]][0] += entry["selector"]["selected"]
        cell_counts[entry["cell"]][1] += entry["selector"]["productive"]
        if entry["active"]:
            grouped[entry["cell"]].append(entry)
    streaks = {}
    final = None
    for record in progress:
        final = record
        census = record["selector"].get("draws_by_cell")
        for tier, counts in record["selector"]["tier_runs"].items():
            previous = streaks.get(tier)
            if previous is None or previous["yields"] != counts["yields"]:
                streaks[tier] = {"yields": counts["yields"], "start": record["executions"], "start_draws": counts["draws"], "boundary": None}
            streak = streaks[tier]
            if census is not None and streak["boundary"] is None:
                streak["boundary"] = {"executions": record["executions"], "tier_draws": counts["draws"],
                                      "cells": {cell: draws for cell, draws in census.items() if cell_tiers.get(cell) == tier}}
    if final is None:
        raise ValueError("empty progress log")
    if final["executions"] != metadata["header"]["executions"]:
        raise ValueError("progress does not end at the checkpoint execution")
    final_draws = final["selector"].get("draws_by_cell")
    if final_draws is None or final_draws != metadata["draws_by_cell"]:
        raise ValueError("final progress cell draws differ from the checkpoint")
    if final["selector"]["tier_runs"] != metadata["tier_runs"]:
        raise ValueError("final progress tier counters differ from the checkpoint")
    tiers = []
    for tier, end in final["selector"]["tier_runs"].items():
        if not end.get("dry", 0):
            continue
        streak = streaks[tier]
        boundary = streak["boundary"]
        if boundary and boundary["executions"] == final["executions"]:
            boundary = None
        interval_draws = end["draws"] - boundary["tier_draws"] if boundary else None
        if interval_draws is not None and interval_draws < 0:
            raise ValueError("tier draw totals decreased")
        cells, total = [], 0
        for cell, draws in final_draws.items():
            if cell_tiers.get(cell) != tier:
                continue
            if boundary:
                delta = draws - boundary["cells"].get(cell, 0)
                if delta < 0:
                    raise ValueError("cell draw totals decreased")
                if delta:
                    cells.append({"cell": cell, "draws": delta})
                    total += delta
            elif draws:
                cells.append({"cell": cell, "draws": None, "lifetime_draws": draws})
        cells.sort(key=lambda row: (-(row["draws"] if boundary else row["lifetime_draws"]), row["cell"]))
        for cell in cells:
            retained = grouped[cell["cell"]]
            selected, productive = cell_counts[cell["cell"]]
            complete = selected == final_draws[cell["cell"]]
            cell.update({"entry_draw_accounting_complete": complete,
                         "retaining_draws_in_checkpoint_metadata": productive,
                         "verified_zero_retention_draws": complete and productive == 0,
                         "interval_retention_yield": 0 if complete and productive == 0 else None})
            identities = {hashlib.sha256(canonical(e["snapshot"]).encode()).hexdigest()
                          for e in retained if e.get("snapshot") is not None}
            cell.update({"share_of_attributed_tier_draws": cell["draws"] / total if total else None,
                         "share_of_tier_draws": cell["draws"] / interval_draws if interval_draws else None,
                         "retained_entries": len(retained), "distinct_snapshot_states": len(identities),
                         "missing_snapshots": sum(e.get("snapshot") is None for e in retained)})
        tiers.append({"tier": tier, "selector_dry_draws": end["dry"],
                      "observed_draws_without_tier_yield": end["draws"] - streak["start_draws"],
                      "zero_yield_observed_from_execution": streak["start"],
                      "cell_draw_window_start": boundary["executions"] if boundary else None,
                      "cell_draw_window_end": final["executions"],
                      "cell_attribution": "observed_zero_yield_interval" if boundary else "unavailable; lifetime draws shown only for orientation",
                      "tier_interval_draws": interval_draws,
                      "attributed_cell_draws": total if boundary else None,
                      "cells": cells,
                      "verified_zero_retention_cells": [row["cell"] for row in cells if row["verified_zero_retention_draws"]],
                      "top_cell_variation": variation(grouped[cells[0]["cell"]], requested) if cells else None})
    return {"format": "nes-stall-report-v1", "checkpoint_sha256": metadata["checkpoint_sha256"],
            "yield_definition": "tier yields count new active cells and carried-in preference wins at that tier; outgoing retention yield is certified separately only where entry accounting is complete",
            "executions": final["executions"], "historical_entries_dropped": metadata["historical_entries_dropped"],
            "limitations": ["Only resident retained snapshots are inspected; missing states are not reconstructed.",
                            "No causal or key-discovery claim follows from variation ranks.",
                            "Per-cell retention yield over an interval is unavailable unless complete lifetime counters prove zero; tier yield alone cannot certify outgoing cell productivity.",
                            "Sparse cell telemetry can leave the final zero-yield interval unattributed.",
                            "Cell shares exclude cells no longer represented in checkpoint metadata; compare attributed and tier draws.",
                            "Counters and execution count match the inputs; this is not a cryptographic binding between checkpoint and log."],
            "tiers": tiers}


def file_hash(path):
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def diagnostic(value):
    name, separator, address = value.partition("=")
    if not separator:
        address, name = name, None
    number = int(address.removeprefix("$"), 16)
    if not (0 <= number < 0x800 or 0x6000 <= number < 0x8000):
        raise argparse.ArgumentTypeError("diagnostic address must be in work or cartridge RAM")
    return (f"${number:04X}", name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("checkpoint", type=Path)
    parser.add_argument("progress", type=Path)
    parser.add_argument("--game", default="auto", help="workload name understood by the supplied inspector (default: auto)")
    parser.add_argument("--inspector", type=Path, default=Path(__file__).parent / "target/release/nes-checkpoint-inspect")
    parser.add_argument("--byte", action="append", default=[], type=diagnostic, metavar="[NAME=]HEX")
    args = parser.parse_args()
    with tempfile.TemporaryFile(mode="w+t") as output:
        process = subprocess.run([str(args.inspector.resolve()), args.game, str(args.checkpoint)], stdout=output)
        if process.returncode:
            raise ValueError("checkpoint inspector failed")
        output.seek(0)
        metadata_line = output.readline()
        if not metadata_line:
            raise ValueError("checkpoint inspector produced no metadata")
        metadata = json.loads(metadata_line)
        if metadata.get("format") != "nes-checkpoint-inspection-v1":
            raise ValueError("unrecognized checkpoint inspection format")
        entries = [json.loads(line) for line in output]
    result = report(metadata, entries, read_progress(args.progress), args.byte)
    result["progress_sha256"] = file_hash(args.progress)
    json.dump(result, sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError) as error:
        print(f"stall report: {error}", file=sys.stderr)
        sys.exit(1)
