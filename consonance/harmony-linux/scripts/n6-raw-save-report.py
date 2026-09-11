#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare diagnostic save-area bytes; differences are evidence, not a pass."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path

NAMES = ("FXSAVE", "FXSAVE64", "XSAVE", "XSAVEOPT", "XSAVEC", "XSAVES")
MAX_LOG_BYTES = 16 * 1024 * 1024
OPERATION = re.compile(
    r"N6_OPERATION arch=x86_64 row=x86-xsave-image operation=([1-6])/6 "
    r"name=(\S+) result=(value:[0-9a-f]{16}:mem:[0-9a-f]{16}|signal:[0-9]+)"
)
RAW = re.compile(
    r"N6_RAW_SAVE name=(\S+) value=([0-9a-f]{16}) hash=([0-9a-f]{16}) bytes=([0-9a-f]{8192})"
)
# The live collector stops as soon as this prefix reaches the serial port.
# Its retained log may end here, before the remainder of the footer is sent.
DONE = re.compile(r"N6_GUEST_OK arch=x86_64(?: .*)?")


def fnv(data: bytes) -> str:
    # Match the existing guest's exact seed, including its historical spelling.
    value = 1469598103934665603
    for byte in data:
        value = ((value ^ byte) * 1099511628211) & ((1 << 64) - 1)
    return f"{value:016x}"


def read_capture(path: Path) -> dict:
    with path.open("rb") as stream:
        data = stream.read(MAX_LOG_BYTES + 1)
    if len(data) > MAX_LOG_BYTES:
        raise ValueError(f"{path}: diagnostic log exceeds its byte limit")
    operations, captures = {}, {}
    completed = 0
    for line in data.decode("utf-8").splitlines():
        if "N6_OPERATION arch=x86_64 row=x86-xsave-image " in line:
            line = line[line.index("N6_OPERATION "):]
            match = OPERATION.fullmatch(line)
            if match is None:
                raise ValueError(f"{path}: malformed save operation")
            ordinal, name, result = match.groups()
            if name != NAMES[int(ordinal) - 1] or name in operations:
                raise ValueError(f"{path}: repeated or misordered save operation")
            operations[name] = result
        elif "N6_RAW_SAVE " in line:
            line = line[line.index("N6_RAW_SAVE "):]
            match = RAW.fullmatch(line)
            if match is None:
                raise ValueError(f"{path}: malformed raw save record")
            name, value, recorded_hash, raw = match.groups()
            if name not in NAMES or name in captures:
                raise ValueError(f"{path}: repeated or unknown raw save record")
            decoded = bytes.fromhex(raw)
            if fnv(decoded) != recorded_hash:
                raise ValueError(f"{path}: raw bytes do not match their hash")
            captures[name] = (value, recorded_hash, decoded)
        elif "N6_GUEST_OK " in line:
            line = line[line.index("N6_GUEST_OK "):]
            if DONE.fullmatch(line) is None:
                raise ValueError(f"{path}: malformed completion record")
            completed += 1
    if completed != 1 or tuple(operations) != NAMES:
        raise ValueError(f"{path}: incomplete or reordered diagnostic sweep")
    successful = {name for name, result in operations.items() if result.startswith("value:")}
    if set(captures) != successful or "XSAVE" not in successful:
        raise ValueError(f"{path}: missing, unexpected or unavailable raw XSAVE capture")
    for name, (value, recorded_hash, _) in captures.items():
        if operations[name] != f"value:{value}:mem:{recorded_hash}":
            raise ValueError(f"{path}: capture differs from the original operation observation")
    return {"path": str(path), "log_sha256": hashlib.sha256(data).hexdigest(),
            "operations": operations, "captures": captures}


def compare(first: Path, second: Path) -> dict:
    left, right = read_capture(first), read_capture(second)
    rows = {}
    for name in NAMES:
        a, b = left["captures"].get(name), right["captures"].get(name)
        differences = ([[index, x, y] for index, (x, y) in enumerate(zip(a[2], b[2])) if x != y]
                       if a is not None and b is not None else None)
        row = {"first_result": left["operations"][name], "second_result": right["operations"][name],
               "equal": left["operations"][name] == right["operations"][name] and
                        (a is None and b is None or a is not None and b is not None and a[2] == b[2]),
               "byte_differences": differences,
               "first_byte_difference": differences[0][0] if differences else None}
        for label, capture in (("first", a), ("second", b)):
            row[label + "_raw_sha256"] = hashlib.sha256(capture[2]).hexdigest() if capture else None
        rows[name] = row
    return {"format": "harmony-n6-raw-save-diagnostic-v1",
            "equal": all(row["equal"] for row in rows.values()),
            "first_log": {"path": left["path"], "sha256": left["log_sha256"]},
            "second_log": {"path": right["path"], "sha256": right["log_sha256"]},
            "operations": rows,
            "scope": "instrumented guest observations; extra capture instructions may perturb timing"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("first", type=Path)
    parser.add_argument("second", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        result = compare(args.first, args.second)
    except (OSError, UnicodeError, ValueError) as error:
        result = {"format": "harmony-n6-raw-save-diagnostic-v1", "error": str(error)}
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return 1 if "error" in result else 0


if __name__ == "__main__":
    raise SystemExit(main())
