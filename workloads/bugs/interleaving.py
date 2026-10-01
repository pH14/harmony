#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run interleaving cases sequentially and distinguish case bugs from harness failures."""

import argparse
import fcntl
import itertools
import json
from pathlib import Path
import subprocess
import sys
import time
import uuid

CASES = {
    "lost-update": ("category", "lost_update", "the counter holds every finished increment"),
    "torn-read": ("category", "torn_read", "a copied record has matching fields"),
    "duplicate-request": ("category", "duplicate_request", "each request is applied at most once"),
    "missed-wakeup": ("category", "missed_wakeup", "a queued item never loses its wakeup"),
    "stale-lease": ("category", "stale_lease", "store writes never go backwards in fencing tokens"),
    "aba-reuse": ("category", "aba_reuse", "the stack never resurrects an owned node"),
    "stale-cache": ("toys", "stale_cache", "a filled cache never predates its invalidation"),
    "double-vote": ("toys", "double_vote", "one term never elects two leaders"),
    "mini-wal-reset": ("toys", "mini_wal_reset", "a checkpoint contains one complete log generation"),
}


def verdict(report, summary, assertion, variant, budget, returncode):
    violations = sorted({v for b in report.get("bugs", []) for v in b.get("violations", [])})
    violations += sorted(k for k, a in summary.get("assertions", {}).items()
                         if a.get("failed") and k not in violations)
    seen = summary.get("assertions", {}).get(assertion, {})
    failures = report.get("execution_failures", 0) or report.get("watchdog_cutoffs", 0)
    if returncode or failures or any(v != assertion for v in violations):
        return "ERROR", violations
    if variant == "correct":
        passed = (not violations and not report.get("bugs") and not report.get("bug_found")
                  and report.get("executions", 0) >= budget and seen.get("passed")
                  and not report.get("never_satisfied") and not summary.get("never_satisfied"))
        return ("PASS" if passed else "FAIL"), violations
    hits = [b for b in report.get("bugs", []) if assertion in b.get("violations", [])]
    if hits:
        confirmed = all(b.get("confirmed") and assertion in b.get("replay", {}).get("violations", [])
                        for b in hits)
        return ("FOUND" if confirmed else "UNCONFIRMED"), violations
    if report.get("executions", 0) < budget or not seen.get("passed"):
        return "ERROR", violations
    return "MISS", violations


def self_test():
    import unittest

    class VerdictTests(unittest.TestCase):
        def test_controls_need_budget_and_oracle(self):
            report = dict(executions=5000, bugs=[], bug_found=False)
            summary = {"assertions": {"case": {"passed": True}}}
            self.assertEqual(verdict(report, summary, "case", "correct", 5000, 0)[0], "PASS")
            self.assertEqual(verdict({**report, "executions": 4999}, summary, "case", "correct", 5000, 0)[0], "FAIL")
            self.assertEqual(verdict(report, {}, "case", "correct", 5000, 0)[0], "FAIL")

        def test_setup_death_is_not_a_case_hit(self):
            report = {"bugs": [{"violations": ["node exit"], "confirmed": True}]}
            self.assertEqual(verdict(report, {}, "case", "buggy", 5000, 0)[0], "ERROR")

        def test_case_hits_need_replay(self):
            hit = {"violations": ["case"], "confirmed": True, "replay": {"violations": ["case"]}}
            self.assertEqual(verdict({"bugs": [hit]}, {}, "case", "buggy", 5000, 0)[0], "FOUND")
            self.assertEqual(verdict({"bugs": [{**hit, "confirmed": False}]}, {}, "case", "buggy", 5000, 0)[0], "UNCONFIRMED")
            self.assertEqual(verdict({"bugs": [hit], "executions": 5000}, {}, "case", "correct", 5000, 0)[0], "FAIL")

        def test_runtime_failure_cannot_pass(self):
            self.assertEqual(verdict({"execution_failures": 1}, {}, "case", "correct", 5000, 0)[0], "ERROR")

    result = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(VerdictTests))
    return int(not result.wasSuccessful())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--case", action="append", choices=CASES)
    parser.add_argument("--variant", choices=("both", "correct", "buggy"), default="both")
    parser.add_argument("--seeds", nargs="+", type=int, default=[1, 2, 3, 4, 5])
    parser.add_argument("--noise", nargs="+", type=int, default=[0])
    parser.add_argument("--executions", type=int, default=5000)
    parser.add_argument("--ram-mib", type=int, default=256)
    parser.add_argument("--cli", type=Path)
    parser.add_argument("--kernel", type=Path)
    parser.add_argument("--initramfs", type=Path)
    parser.add_argument("--image-dir", type=Path, default=Path("target/interleaving/images"))
    parser.add_argument("--out", type=Path, default=Path("target/interleaving/runs"))
    parser.add_argument("--build", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if not all((args.cli, args.kernel, args.initramfs)):
        parser.error("--cli, --kernel and --initramfs are required")
    root = Path(__file__).resolve().parents[2]
    if not (root / "Cargo.toml").exists():
        root = Path.cwd()
    if args.executions < 1 or args.ram_mib < 1 or any(n < 0 or n > 32 for n in args.noise):
        parser.error("positive execution/RAM budgets and noise in 0..32 are required")
    target = (root / "target").resolve()
    out = args.out.resolve()
    if not out.is_relative_to(target):
        parser.error("--out must be under this repository's target directory")
    args.image_dir = args.image_dir.resolve()
    args.image_dir.mkdir(parents=True, exist_ok=True)
    out.mkdir(parents=True, exist_ok=True)
    cases = args.case or list(CASES)
    variants = ["correct", "buggy"] if args.variant == "both" else [args.variant]
    rows = []
    with (target / "interleaving.lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            parser.error("another interleaving runner holds the four-VM slot")
        if args.build:
            for case in cases:
                folder = root / "workloads/bugs" / CASES[case][0] / case
                tag = f"harmony-interleaving-{case}"
                subprocess.run(["docker", "build", "--platform", "linux/arm64", "-f", str(folder / "image/Dockerfile"), "-t", tag, str(root)], check=True)
                subprocess.run(["docker", "save", "-o", str(args.image_dir / f"{case}.oci"), tag], check=True)
        campaign = out / (time.strftime("%Y%m%d-%H%M%S") + "-" + uuid.uuid4().hex[:8])
        campaign.mkdir()
        print("| case | variant | noise | seed | first execution | wall seconds | confirmed | result | assertion |", flush=True)
        print("|---|---|---:|---:|---:|---:|---|---|---|", flush=True)
        for case, variant, noise, seed in itertools.product(cases, variants, args.noise, args.seeds):
            image = args.image_dir / f"{case}.oci"
            if not image.is_file():
                parser.error(f"missing image {image}; build it with --build")
            run = campaign / f"{case}-{variant}-n{noise}-s{seed}-{uuid.uuid4().hex[:6]}"
            prefix, assertion = CASES[case][1:]
            command = [str(args.cli.resolve()), "search", "--package", "faults", str(image),
                       "--backend", "consonance", "--kernel", str(args.kernel.resolve()),
                       "--base-initramfs", str(args.initramfs.resolve()), "--seed", str(seed),
                       "--executions", str(args.executions), "--ram-mib", str(args.ram_mib),
                       "--knobs", f"{prefix}.correct={int(variant == 'correct')} {prefix}.noise={noise}",
                       "--out", str(run)]
            start = time.monotonic()
            with run.with_suffix(".console.txt").open("w") as log:
                result = subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT)
            wall = time.monotonic() - start
            def read(name):
                path = run / name
                return json.loads(path.read_text()) if path.exists() else {}
            report, summary = read("report.json"), read("campaign-summary.json")
            status, violations = verdict(report, summary, assertion, variant, args.executions, result.returncode)
            row = dict(case=case, variant=variant, noise=noise, seed=seed, first=report.get("first_bug_execution"),
                       wall_seconds=round(wall, 3), executions=report.get("executions", 0),
                       confirmed=status == "FOUND", status=status, violations=violations,
                       assertion=assertion, out=str(run), command=command,
                       park_sites=summary.get("park_sites", {}), park_reads=summary.get("park_reads", {}),
                       park_thresholds=summary.get("park_thresholds", {}),
                       never_satisfied=summary.get("never_satisfied", []),
                       search_ns=summary.get("telemetry", {}).get("search_ns"))
            rows.append(row)
            with (campaign / "results.jsonl").open("a") as records:
                records.write(json.dumps(row) + "\n")
            print(f"| {case} | {variant} | {noise} | {seed} | {row['first'] or '—'} | {wall:.2f} | {'yes' if row['confirmed'] else '—'} | {status} | {', '.join(violations) or '—'} |", flush=True)
            if status in ("ERROR", "FAIL", "UNCONFIRMED"):
                print(f"Inspect {run.with_suffix('.console.txt')} before continuing.", file=sys.stderr)
                return 1
    print(f"Run records: {campaign}", flush=True)
    return int(any(r["status"] != "PASS" and r["status"] != "FOUND" for r in rows))


if __name__ == "__main__":
    raise SystemExit(main())
