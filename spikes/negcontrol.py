# SPDX-License-Identifier: AGPL-3.0-or-later
"""Shared planted-failure (negative-control) harness for the spike evidence graders.

A grader without a negative-control fixture proving it can go RED is not a gate (work order
hm-537). This module is that harness, written once and shared across every spike whose grader is
a script or binary with a CLI: the ARM determinism comparators (`aa1c-determinism-check.py`,
`aa3-determinism-compare.py`), the AMD floor checker (`amd-epyc/schemas/check-floors.py`), and any
future spike gate. (The Rust `floor-check` grader has its own in-process incarnation of the same
discipline — `schemas/floor-check/src/fixtures.rs` + the accept/reject suite — because it is
unit-testable in Rust; this module is for graders exercised through a CLI.)

# Arming a new gate is five lines

Start from a KNOWN-GOOD retained fixture, apply exactly ONE mutation, run the grader, assert RED:

    good = [record(), record()]                 # a fixture the grader accepts
    write_records_json(tmp / "bad.json", mutate(good, 0, count_n2=999))   # one field wrong
    r = run_grader(CHECK_FLOORS, "exactness", "--min-reps", "1", "--records", tmp / "bad.json")
    self.assertNotEqual(r.returncode, 0)         # the gate MUST go red

That is the whole contract. `write_run_set` builds a run-set DIRECTORY (manifest + records.jsonl,
sha256 pinned) for graders that take a directory; `write_records_json` writes a bare JSON array for
graders that take a records file; `mutate`/`drop` apply a single change; `run_grader` runs the
grader and hands back its exit code, streams, and parsed JSON stdout.
"""

import hashlib
import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


@dataclass
class GraderResult:
    """The outcome of running a grader: its exit code and captured streams."""

    returncode: int
    stdout: str
    stderr: str

    def json(self):
        """Parse the grader's stdout as JSON (the ARM comparators emit a stable-JSON report).

        Returns `None` if stdout is not JSON, so a caller can assert on the exit code alone for a
        grader (like check-floors.py) whose stdout is human lines, not JSON."""
        try:
            return json.loads(self.stdout)
        except json.JSONDecodeError:
            return None


def run_grader(script, *args, cwd=None, python=True):
    """Run a grader as a subprocess and capture its result.

    `script` is the grader path; `args` are its CLI arguments (each stringified). `python=True`
    (the default) runs it under the current interpreter — the graders here are Python scripts;
    pass `python=False` to exec a compiled binary directly."""
    argv = [sys.executable] if python else []
    argv.append(str(script))
    argv.extend(str(a) for a in args)
    proc = subprocess.run(
        argv, check=False, capture_output=True, text=True, cwd=cwd
    )
    return GraderResult(proc.returncode, proc.stdout, proc.stderr)


def _encode_jsonl(records):
    """Encode records as canonical one-object-per-line JSONL bytes (stable key order)."""
    return b"".join(
        (json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n").encode()
        for record in records
    )


def write_run_set(root, name, records, *, run_set_id=None, condition=None,
                  records_file="records.jsonl", **manifest_extra):
    """Write a run-set DIRECTORY under `root/name`: a `records.jsonl` plus a `run-set.json`
    manifest whose `records_sha256` and `attempted` are pinned to the emitted bytes.

    `run_set_id` and `condition` default to `name`, so two differently-named lanes are, by
    construction, distinct run-sets under distinct conditions — the provenance a determinism
    comparator attests. Pass them explicitly to model a specific lane (a pinned-solo reference vs a
    co-tenant run), or pass the SAME directory to a comparator twice to plant the self-comparison
    negative control. `manifest_extra` overrides or adds any manifest field."""
    run_set = Path(root) / name
    run_set.mkdir(parents=True, exist_ok=True)
    encoded = _encode_jsonl(records)
    (run_set / records_file).write_bytes(encoded)
    manifest = {
        "attempted": len(records),
        "records_file": records_file,
        "records_sha256": hashlib.sha256(encoded).hexdigest(),
        "run_set_id": run_set_id if run_set_id is not None else name,
        "condition": condition if condition is not None else name,
    }
    manifest.update(manifest_extra)
    (run_set / "run-set.json").write_text(json.dumps(manifest), encoding="utf-8")
    return run_set


def write_records_json(path, records):
    """Write `records` as a JSON array to `path` (the shape check-floors.py loads). Returns the
    path so a caller can pass it straight to `run_grader`."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(records), encoding="utf-8")
    return path


def mutate(records, index, **changes):
    """Return a COPY of `records` with `records[index]` updated by `changes` — the single-field
    mutation a negative control is built from. The original list is left untouched, so one
    known-good fixture can seed many controls."""
    copied = [dict(r) for r in records]
    copied[index].update(changes)
    return copied


def drop(records, index, *keys):
    """Return a COPY of `records` with `keys` removed from `records[index]` — the 'a field the
    grader compares is simply absent' mutation (e.g. the hm-cte symmetric-omission control)."""
    copied = [dict(r) for r in records]
    for key in keys:
        copied[index].pop(key, None)
    return copied
