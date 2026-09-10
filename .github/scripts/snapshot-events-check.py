#!/usr/bin/env python3
"""Compare retained raw SDK event sequences at the three measured endpoints."""
import hashlib
import json
from pathlib import Path
import sys

reports = Path(sys.argv[1])
case = sys.argv[2]
endpoints = [json.loads((reports / f"{case}.{name}.json").read_text())
             for name in ("inspect", "run-whole", "run-split-2")]
identities = [(e["state_hash"], e["virtual_time_nanos"]) for e in endpoints]
assert identities[0] == identities[1] == identities[2], identities
engine_hash = identities[0][0]
original = list(reports.glob(f"original-sdk-events-{engine_hash}-*.json"))
continuations = list(reports.glob(f"continuation-sdk-events-{engine_hash}-*.json"))
assert original, "missing original event stream"
assert len(continuations) >= 2, "missing independently captured cold/split event streams"
expected = json.loads(original[0].read_text())
for path in original + continuations:
    actual = json.loads(path.read_text())
    assert actual == expected, f"guest event mismatch: {path}"
serialized = json.dumps(expected, separators=(",", ":")).encode()
print(f"Identical original/cold/split raw SDK events: {len(expected)} events, "
      f"sha256={hashlib.sha256(serialized).hexdigest()}, "
      f"{len(original) + len(continuations)} independent captures")
