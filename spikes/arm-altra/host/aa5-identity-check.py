#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-5(c) same-seed identity floor-check. Two `arm-spike linux-boot` runs of the
# SAME pinned Image/initramfs must agree bit-identically on console and on the
# landing-anchored machine state digest. Per §Evidence integrity #2 the console
# hash is RECOMPUTED from each retained transcript, never read back from the
# harness's own summary line; the state digest exists only machine-side and is
# cross-checked between the two runs plus required to be well-formed.
#
#   aa5-identity-check.py --run-a <dir> --run-b <dir> --expect mismatch? --out verdict.json
#
# Each run dir holds: stdout.txt (the harness summary line) and console.bin
# (the retained transcript written by the harness). Exit 0 iff the verdict is
# PASS (or, with --expect-mismatch, iff the runs genuinely differ — the
# negative-control mode that proves this checker can see a difference).
import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

# Transcripts and summaries are kilobytes; this cap guards the checker from OOMing
# on a malformed/oversized run dir before it has a chance to validate anything (F6).
MAX_RUN_FILE = 64 * 1024 * 1024  # 64 MiB

REQUIRED_MARKERS = [b"HARMONY_AA5_CLOCKSOURCE_OK", b"HARMONY_AA5_READY"]
SUMMARY_KEYS = [
    "exits",
    "console_bytes",
    "console_sha256",
    "image_sha256",
    "initramfs_sha256",
    "pvclock_publications",
    "pvclock_max_gap_work",
    "pvclock_last_work",
    "pvclock_gpa",
    "guest_clock_hz",
    "clockevent_assertions",
    "clockevent_acks",
    "clockevent_max_lateness_ticks",
    "state_digest",
]


def _read_bounded(path: Path, *, binary: bool):
    # Bound the read by the file size before pulling bytes into memory (F6): a run
    # dir is operator-supplied, and read_text/read_bytes would otherwise slurp an
    # arbitrarily large file whole.
    size = path.stat().st_size
    if size > MAX_RUN_FILE:
        raise SystemExit(f"FAIL: {path}: {size} bytes exceeds the {MAX_RUN_FILE}-byte run-file cap")
    return path.read_bytes() if binary else path.read_text()


def load_run(run_dir: Path):
    stdout = _read_bounded(run_dir / "stdout.txt", binary=False)
    console = _read_bounded(run_dir / "console.bin", binary=True)
    fields = dict(re.findall(r"([a-z_0-9]+)=([^\s]+)", stdout))
    missing = [k for k in SUMMARY_KEYS if k not in fields]
    if missing:
        raise SystemExit(f"FAIL: {run_dir}: summary line missing {missing}")
    recomputed = hashlib.sha256(console).hexdigest()
    if recomputed != fields["console_sha256"]:
        raise SystemExit(
            f"FAIL: {run_dir}: retained transcript sha256 {recomputed} != summary "
            f"{fields['console_sha256']} — transcript and summary disagree"
        )
    for marker in REQUIRED_MARKERS:
        if marker not in console:
            raise SystemExit(f"FAIL: {run_dir}: console lacks {marker.decode()}")
    # A sha256 is exactly 64 hex digits; require the full width (F5). The old
    # `{16,}` accepted a truncated digest, which would weaken the cross-run
    # comparison below to a 16-nibble prefix.
    if not re.fullmatch(r"(sha256:)?[0-9a-f]{64}", fields["state_digest"]):
        raise SystemExit(f"FAIL: {run_dir}: malformed state_digest")
    return fields, recomputed, console


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-a", required=True, type=Path)
    ap.add_argument("--run-b", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument(
        "--expect-mismatch",
        action="store_true",
        help="negative-control mode: succeed only if the runs differ",
    )
    args = ap.parse_args()

    a, a_sha, a_console = load_run(args.run_a)
    b, b_sha, b_console = load_run(args.run_b)

    if a["image_sha256"] != b["image_sha256"] or a["initramfs_sha256"] != b["initramfs_sha256"]:
        raise SystemExit("FAIL: the two runs did not boot the same pinned artifacts")

    compared = {}
    identical = True
    for key in SUMMARY_KEYS:
        if key in ("image_sha256", "initramfs_sha256"):
            continue
        same = a[key] == b[key]
        compared[key] = {"a": a[key], "b": b[key], "identical": same}
        identical = identical and same
    console_identical = a_console == b_console
    compared["console_bytes_bitwise"] = {
        "a_sha256": a_sha,
        "b_sha256": b_sha,
        "identical": console_identical,
    }
    identical = identical and console_identical

    verdict = {
        "check": "aa5-same-seed-identity",
        "expect_mismatch": args.expect_mismatch,
        "identical": identical,
        "fields": compared,
        "result": None,
    }
    if args.expect_mismatch:
        verdict["result"] = "PASS(negative-control)" if not identical else "FAIL(vacuous)"
    else:
        verdict["result"] = "PASS" if identical else "FAIL"
    args.out.write_text(json.dumps(verdict, indent=1, sort_keys=True) + "\n")
    print(f"{verdict['result']}: {args.out}")
    sys.exit(0 if verdict["result"].startswith("PASS") else 1)


if __name__ == "__main__":
    main()
