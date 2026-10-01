#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Require timer progress and byte-identical logs and execution records."""

import argparse
import hashlib
import json
from pathlib import Path


def verify(first, second):
    expected = [f"HARMONY_LANGUAGE_MARKER {marker:02}".encode() for marker in range(1, 21)]
    logs = []
    records = []
    for directory in (first, second):
        log = (directory / "serial.log").read_bytes()
        markers = [line for line in log.splitlines() if line.startswith(b"HARMONY_LANGUAGE_MARKER ")]
        if markers != expected:
            raise ValueError(f"{directory}: expected twenty ordered markers, found {len(markers)}")
        record = json.loads((directory / "run.json").read_text())
        if record["container_rc"] != 0:
            raise ValueError(f"{directory}: fixture failed")
        if record["serial_sha256"] != hashlib.sha256(log).hexdigest():
            raise ValueError(f"{directory}: serial digest mismatch")
        logs.append(log)
        records.append(record)
    if logs[0] != logs[1]:
        raise ValueError("serial logs differ")
    if records[0] != records[1]:
        raise ValueError("execution records differ")
    return {"markers": 20, "identical_serial": True, "identical_run": True,
            "serial_sha256": records[0]["serial_sha256"]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("first", type=Path)
    parser.add_argument("second", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    report = verify(args.first, args.second)
    encoded = json.dumps(report, sort_keys=True) + "\n"
    if args.output:
        args.output.write_text(encoded)
    print(encoded, end="")


if __name__ == "__main__":
    main()
