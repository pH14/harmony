#!/usr/bin/env python3
"""Q02: legacy byte compatibility, followed by bounded optional-policy replay."""
import argparse
import json
from itertools import zip_longest
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True, help="Frozen candidate source")
    parser.add_argument("--experiment", type=Path, required=True)
    parser.add_argument("--recheck", action="store_true", help="Verify completed outputs without rerunning search")
    args = parser.parse_args()
    root, experiment = args.root, args.experiment
    base = json.loads((root / "benchmarks/search/retention-theory/b01.json").read_text())
    expected = {
        "smb-full": "12945d74e4fc4ac83dae39c3419c2d0785acc241afd15407ce04b41c4bf42e32",
        "mm2-metal": "2b94c50e70fc971007730fc17a8dc1a3402cc9fdcc2cc8f6953b6dd4e194b49d",
    }
    records = []
    for policy in [None, "resource_extremes_2_v1", "resource_coverage_2_v1"]:
        label = policy or "legacy"
        manifest = json.loads(json.dumps(base))
        manifest["id"] = f"retention-theory-q02-{label}"
        if policy:
            manifest["search"]["slot_retention"] = policy
        if policy == "resource_extremes_2_v1":
            manifest["cases"] = [case for case in manifest["cases"] if case["game"] == "mm2"]
        path = experiment / f"q02-{label}.json"
        path.write_text(json.dumps(manifest, indent=2) + "\n")
        out = experiment / "runs" / f"q02-{label}"
        command = [
            "python3", str(root / "benchmarks/search/eval.py"), "run", str(path),
            "--assets", str(experiment / "assets.json"),
            "--binary", str(experiment / "builds/coverage-001/nes-eval"),
            "--build-info", str(experiment / "builds/coverage-001/build-info.json"),
            "--out", str(out), "--jobs", "1", "--cpus", "4",
            "--memory-capacity-mib", "2048", "--finish-seconds", "60",
            "--disk-limit-gib", "4",
        ]
        if not args.recheck:
            subprocess.run(command, check=True)
        assert len(list(out.glob("*/summary.json"))) == len(manifest["cases"]), "missing cells"
        for summary_path in sorted(out.glob("*/summary.json")):
            summary = json.loads(summary_path.read_text())
            result = summary["result"]
            assert summary["status"] == "complete"
            assert result["verification"] == "campaign"
            if policy is None:
                assert result["stream_sha256"] == expected[summary["case"]], "legacy stream changed"
            elif summary["case"] == "smb-full":
                # The requested policy changes its recorded header even when
                # the workload has no axes. Compare that exact allowed
                # difference, then every remaining byte.
                control = experiment / "runs/q02-legacy" / summary["cell"] / "campaign/stream.jsonl"
                candidate = summary_path.parent / "campaign/stream.jsonl"
                with control.open("rb") as a, candidate.open("rb") as b:
                    ah, bh = json.loads(next(a)), json.loads(next(b))
                    assert bh.pop("slot_retention") == policy
                    assert "slot_retention" not in ah and ah == bh
                    assert all(x == y for x, y in zip_longest(a, b)), "fallback decisions changed"
            diagnostics = (summary.get("last_progress") or {}).get("retention_diagnostics") or {}
            if policy and summary["case"] == "mm2-metal":
                assert diagnostics.get("alternative_admissions", 0) > 0, "optional mechanism not exercised"
            record = {"policy": policy, "case": summary["case"], "result": result,
                      "retention_diagnostics": diagnostics,
                      "peak_rss_bytes": summary.get("peak_process_tree_rss_bytes_sampled"),
                      "elapsed_seconds": summary.get("elapsed_seconds")}
            records.append(record)
            print(json.dumps({"policy": policy, "case": summary["case"], "status": "qualified",
                              "stream": result["stream_sha256"], "retention": diagnostics}), flush=True)
        (experiment / "runs/q02-results.json").write_text(json.dumps(records, indent=2) + "\n")


if __name__ == "__main__":
    main()
