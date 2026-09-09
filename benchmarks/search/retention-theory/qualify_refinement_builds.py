#!/usr/bin/env python3
"""Q03: unchanged SMB/MM2 streams from default and Metroid-refined builds."""
import argparse
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experiment", type=Path, required=True)
    experiment = parser.parse_args().experiment.resolve()
    source = experiment / "source-refinement-001"
    expected = {"smb-full": "12945d74e4fc4ac83dae39c3419c2d0785acc241afd15407ce04b41c4bf42e32",
                "mm2-metal": "2b94c50e70fc971007730fc17a8dc1a3402cc9fdcc2cc8f6953b6dd4e194b49d"}
    records = []
    for name in ["refinement-default-001", "refinement-001"]:
        build = experiment / "builds" / name
        out = experiment / "runs" / f"q03-{name}"
        subprocess.run([
            "python3", str(source / "benchmarks/search/eval.py"), "run",
            str(source / "benchmarks/search/retention-theory/b01.json"),
            "--assets", str(experiment / "assets.json"), "--binary", str(build / "nes-eval"),
            "--build-info", str(build / "build-info.json"), "--out", str(out),
            "--jobs", "1", "--cpus", "4", "--memory-capacity-mib", "2048",
            "--finish-seconds", "60", "--disk-limit-gib", "4",
        ], check=True)
        paths = sorted(out.glob("*/summary.json"))
        assert len(paths) == 2
        for path in paths:
            s = json.loads(path.read_text())
            assert s["status"] == "complete" and s["result"]["verification"] == "campaign"
            assert s["result"]["stream_sha256"] == expected[s["case"]], "historical stream changed"
            records.append({"build": json.loads((build / "build-info.json").read_text()),
                            "case": s["case"], "result": s["result"],
                            "elapsed_seconds": s["elapsed_seconds"],
                            "peak_rss_bytes": s["peak_process_tree_rss_bytes_sampled"]})
            print(json.dumps({"build": name, "case": s["case"], "status": "qualified"}), flush=True)
        (experiment / "runs/q03-results.json").write_text(json.dumps(records, indent=2) + "\n")


if __name__ == "__main__":
    main()
