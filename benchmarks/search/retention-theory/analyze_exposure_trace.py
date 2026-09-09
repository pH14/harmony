#!/usr/bin/env python3
"""Extract conservative continuation evidence from already verified streams."""
import argparse
import hashlib
import json
from pathlib import Path


def validate_header(header, game):
    formats = {"metroid": "metroid-quicknes-campaign-stream-v4", "mm2": "mm2-quicknes-campaign-stream-v1"}
    assert header["format"] == formats[game], "unsupported workload stream identity"
    assert header["origin_kind"] == "genesis" and header["resume_actions"] == 0
    assert header.get("origin_path") is None and header.get("origin_archive_sha256") is None


def summarize(records):
    born, later_boundary, jobs, skips = set(), set(), set(), set()
    executions = skip_records = 0
    for record in records:
        parent = record["parent_id"]
        assert parent == 0 or parent in born, "parent precedes neither genesis nor a recorded birth"
        if record["event"] == "skip":
            skips.add(parent)
            skip_records += 1
            continue
        assert record["event"] == "job"
        executions += 1
        assert record["sequence"] == executions
        jobs.add(parent)
        decisions = record["decisions"]
        for index, decision in enumerate(decisions):
            assert decision["decision"] in {"retained", "duplicate", "rejected", "probe_refused", "victory"}
            if decision["decision"] == "retained":
                identity = decision["id"]
                assert identity > 0 and identity not in born, "duplicate retained birth"
                born.add(identity)
                # Retained is the final decision emitted for its action. Any
                # later decision therefore proves a later action was executed.
                if index + 1 < len(decisions):
                    later_boundary.add(identity)
    unreferenced = born - jobs - skips
    never_job_parent = born - jobs
    known_continued_unreferenced = unreferenced & later_boundary
    return {"executed_admitted_jobs": executions, "duplicate_skip_records": skip_records,
            "created_entries_excluding_genesis": len(born),
            "created_entries_referenced_as_job_parents": len(born & jobs),
            "created_entries_referenced_only_by_skips": len((born & skips) - jobs),
            "created_entries_never_referenced_as_parents": len(unreferenced),
            "created_entries_never_job_parents": len(never_job_parent),
            "created_entries_with_later_birth_job_candidate_boundary": len(later_boundary),
            "never_referenced_entries_proven_continued_in_birth_job": len(known_continued_unreferenced),
            "fraction_of_never_referenced_entries_with_proven_birth_continuation": len(known_continued_unreferenced) / len(unreferenced) if unreferenced else None,
            "example_proven_entry_ids": sorted(known_continued_unreferenced)[:8]}


def analyze(root):
    qualified = json.loads((root / "runs/r04b-qualify-results.json").read_text())
    assert qualified["passed"] and len(qualified["records"]) == 5
    rows = []
    for reference in qualified["records"]:
        label = reference["label"]
        files = list((root / f"runs/r04b-qualify-{label}").glob("*/campaign/stream.jsonl"))
        assert len(files) == 1
        path = files[0]
        assert path.stat().st_size <= 128 * 1024**2, "offline input bound exceeded"
        raw = path.read_bytes()
        summary = reference["summary"]
        assert summary["status"] == "complete" and summary["result"]["verification"] == "campaign"
        digest = hashlib.sha256(raw).hexdigest()
        assert digest == summary["result"]["stream_sha256"], "stream differs from verified evidence"
        lines = raw.splitlines()
        header = json.loads(lines[0])
        validate_header(header, summary["search_request"]["game"])
        assert len(lines) <= 100_001
        result = summarize(json.loads(line) for line in lines[1:])
        assert result["executed_admitted_jobs"] == summary["result"]["executions"] == 5000
        rows.append({"label": label, "stream_sha256": digest, "stream_path": str(path.relative_to(root)),
                     "slot_retention": header.get("slot_retention"), "mixture_policy": header["mixture_policy"], **result})
    return {"format": "verified-stream-continuation-lower-bound-v1", "emulator_frames_added": 0,
            "scope": "all five already replay-verified R04b qualification streams, not deep-search exposure estimates",
            "rows": rows, "limitations": [
                "A retained decision is last for its action; a subsequent candidate decision proves at least one later executed action.",
                "Actions without a candidate decision remain invisible, so this deliberately undercounts birth-job continuation.",
                "Parent references include pre-execution duplicate skips, which do not execute a new job.",
                "No-reference classification covers the complete recorded stream, not removal-time counters or the final active set.",
                "A single proven continuation does not establish adequate exploration, useful yield, or absence of starvation.",
                "Unadmitted physical work and the timing of removal versus pending execution are not reconstructed.",
                "This measurement cannot reopen failed retention gates or justify a scheduler intervention by itself."]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experiment", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    a = parser.parse_args()
    report = analyze(a.experiment.resolve())
    a.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps([{"label": x["label"], "never_referenced": x["created_entries_never_referenced_as_parents"],
                       "proven_continued": x["never_referenced_entries_proven_continued_in_birth_job"]} for x in report["rows"]], indent=2))
