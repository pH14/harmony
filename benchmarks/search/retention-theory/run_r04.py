#!/usr/bin/env python3
"""Bounded motion-context qualification and a separately gated development pair."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

QUALITY = "quality_representatives_2_v1"
CONTEXT = "context_representatives_2_v1"
SAMPLE = "representative_job_sample_2_v1"
TERMINAL = "death_or_bcd_underflow_or_ending_v3"
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--phase", choices=["qualify", "development"], required=True)
    a = p.parse_args()
    root = a.experiment.resolve()
    source = root / "source-context-001"
    research = source / "benchmarks/search/retention-theory"
    out_path = root / f"runs/r04-{a.phase}-results.json"
    assert not out_path.exists(), "refusing to overwrite R04"
    report = {"format": "r04-context-retention-v1", "phase": a.phase,
              "scope": "mechanism qualification" if a.phase == "qualify" else "one fresh development seed; not validation",
              "cpus": "0-3", "records": [], "passed": False}

    def save():
        out_path.write_text(json.dumps(report, indent=2) + "\n")

    def run(label, game, policy, build_name):
        build = root / "builds" / build_name
        suite = json.loads((research / ("k01-smoke.json" if game == "metroid" else "b01.json")).read_text())
        suite["id"] = f"retention-theory-r04-{a.phase}-{label}"
        suite["cases"] = [case for case in suite["cases"] if case["game"] == game]
        if game == "metroid":
            suite["search"]["metroid_terminal"] = TERMINAL
        if policy:
            suite["search"]["slot_retention"] = policy
        if a.phase == "development":
            suite["search"].update({"executions": 500000, "frames": 50000000,
                                     "wall_seconds": 1800, "verification": "witness"})
        manifest = root / f"r04-{a.phase}-{label}.json"
        out = root / f"runs/r04-{a.phase}-{label}"
        assert not out.exists()
        manifest.write_text(json.dumps(suite, indent=2) + "\n")
        subprocess.run(["taskset", "-c", "0-3", "python3", str(source / "benchmarks/search/eval.py"),
                        "run", str(manifest), "--assets", str(root / "assets.json"),
                        "--binary", str(build / "nes-eval"), "--build-info", str(build / "build-info.json"),
                        "--out", str(out), "--jobs", "1", "--cpus", "4",
                        "--memory-capacity-mib", "10240", "--finish-seconds", "120", "--disk-limit-gib", "4"],
                       check=True, timeout=suite["search"]["wall_seconds"] + 180)
        paths = list(out.glob("*/summary.json"))
        assert len(paths) == 1
        summary = json.loads(paths[0].read_text())
        record = {"label": label, "game": game, "policy": policy,
                  "build": json.loads((build / "build-info.json").read_text()), "summary": summary}
        census = paths[0].parent / "campaign/retention-census.json"
        if census.exists():
            record["census"] = json.loads(census.read_text())
        report["records"].append(record)
        save()
        assert summary["status"] == "complete", "incomplete cell; preserve and stop"
        result = summary["result"]
        if a.phase == "qualify":
            assert result["verification"] == "campaign" and result["stop_reason"] != "wall_limit"
        print(json.dumps({"label": label, "frames": result["frames_emulated"],
                          "stop_reason": result["stop_reason"], "stream": result["stream_sha256"],
                          "census": record.get("census")}), flush=True)
        return record

    save()
    if a.phase == "qualify":
        default = run("default-metroid", "metroid", None, "context-default-001")
        assert default["summary"]["result"]["stream_sha256"] == "9df5aefec98188306cf68f7b5959a0b4eef88df9a6786b38a31bfad73c5ae94e"
        default_mm2 = run("default-mm2", "mm2", None, "context-default-001")
        assert default_mm2["summary"]["result"]["stream_sha256"] == "2b94c50e70fc971007730fc17a8dc1a3402cc9fdcc2cc8f6953b6dd4e194b49d"
        sample = run("default-sample", "metroid", SAMPLE, "context-default-001")
        old = json.loads((research / "r03-qualify-results.json").read_text())
        expected = next(r for r in old["records"] if r["game"] == "metroid" and r["policy"] == SAMPLE)
        assert sample["summary"]["result"]["stream_sha256"] == expected["summary"]["result"]["stream_sha256"]
        quality = run("quality", "metroid", QUALITY, "context-001")
        context = run("context", "metroid", CONTEXT, "context-001")
        for record in [quality, context]:
            census = record["census"]
            assert census["resident_entries"] == census["with_context"]
            assert census["largest_slot"] <= 2
            assert record["summary"]["last_progress"]["retention_diagnostics"]["alternative_admissions"] > 0
        assert context["census"]["two_same_contexts"] == 0
        report["mechanism_pilot_passed"] = (quality["census"]["two_same_contexts"] >= 10
                                               and context["census"]["two_distinct_contexts"] >= 10)
        assert report["mechanism_pilot_passed"], "no adequate distinguishing opportunity; do not expand"
    else:
        gate_path = root / "runs/r04-qualify-results.json"
        gate = json.loads(gate_path.read_text())
        assert gate["passed"] and gate["mechanism_pilot_passed"]
        motion = json.loads((root / "runs/p04-cache-check/summary.json").read_text())
        assert motion["cached_context_matches_direct_read"] and motion["campaign_key_context_checked"]
        report["qualification_sha256"] = sha(gate_path)
        report["motion_check"] = motion
        digest = sha(root / "builds/context-001/nes-eval")
        assert all(r["build"]["binary_sha256"] == digest for r in gate["records"] if r["label"] in ["quality", "context"])
        run("quality", "metroid", QUALITY, "context-001")
        run("context", "metroid", CONTEXT, "context-001")
    report["passed"] = True
    save()


if __name__ == "__main__":
    main()
