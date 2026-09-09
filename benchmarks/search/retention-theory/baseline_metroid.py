#!/usr/bin/env python3
"""B02: establish the original ARM Metroid streams before key/terminal changes."""
import argparse
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experiment", type=Path, required=True)
    root = parser.parse_args().experiment.resolve()
    records = []
    for stage in ["smoke", "compatibility"]:
        out = root / "runs" / f"b02-{stage}"
        subprocess.run([
            "python3", str(root / "benchmarks/search/eval.py"), "run",
            str(root / f"k01-{stage}.json"), "--assets", str(root / "assets.json"),
            "--binary", str(root / "builds/baseline-001/nes-eval"),
            "--build-info", str(root / "builds/baseline-001/build-info.json"),
            "--out", str(out), "--jobs", "1", "--cpus", "4",
            "--memory-capacity-mib", "10240", "--finish-seconds", "120",
            "--disk-limit-gib", "4",
        ], check=True)
        paths = list(out.glob("*/summary.json"))
        assert len(paths) == 1
        summary = json.loads(paths[0].read_text())
        assert summary["status"] == "complete"
        assert summary["result"]["stop_reason"] == "execution_limit"
        assert summary["result"]["verification"] == ("campaign" if stage == "smoke" else "witness")
        records.append(summary)
        (root / "runs/b02-results.json").write_text(json.dumps(records, indent=2) + "\n")
        print(json.dumps({"stage": stage, "stream": summary["result"]["stream_sha256"],
                          "status": "qualified"}), flush=True)


if __name__ == "__main__":
    main()
