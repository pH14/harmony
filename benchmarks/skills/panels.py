# SPDX-License-Identifier: AGPL-3.0-or-later
"""The three evaluation panels: what each attempt is given, and how its
artifacts are checked afterwards.

A grader reads the frozen workspace, the transcript, and the submitted report.
It never reads the agent's own claim of success. Where a check can only be
answered by reading the agent's prose, it is recorded as a claim and reported
separately from the checks that read artifacts.
"""

from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass, field
from pathlib import Path


@dataclass
class Check:
    """One graded dimension."""

    name: str
    passed: bool
    evidence: str
    kind: str = "artifact"


@dataclass
class Panel:
    """A starting position, an assignment, and the checks its result faces."""

    name: str
    needs_kvm: bool
    tools: list[str]
    bash_allow: list[str]
    prompt: str
    materials: list[str] = field(default_factory=list)


INVESTIGATION_PROMPT = """\
The directory `workspace/` is a Harmony workspace: the durable history a fault
search left behind, including a recorded failure.

Find the recorded failure and explain it with guest evidence.

Use the `harmony` command on your PATH. Read `CLI-README.md` in this directory
for what the command does. Work one command at a time.

Write your answer to `REPORT.md` in this directory. It must state:

* which finding you examined and which property failed,
* what that property claims and which command evaluated it,
* the guest evidence you obtained, cited by the moment and evidence ids the
  CLI returned, and quoting the lines you relied on,
* what you changed, if anything, and whether it changed what you saw,
* what you could not observe, and which declared properties reached no verdict.

Do not claim a cause the evidence does not establish. Do not modify the
recorded finding.
"""

INTEGRATION_PROMPT = """\
Prepare the PostgreSQL source in `source/` for testing in Harmony.

Focus on the correctness of `CREATE INDEX CONCURRENTLY` under concurrent
updates and maintenance. Read `CONTRACT.md` for the behavior that feature is
supposed to have.

Identify and prioritize the properties worth checking, implement checks and
observations for them, and produce a reproducible build and workload image.
Use supported compiler or runtime instrumentation only where it materially
improves Harmony's exploration. Preserve PostgreSQL's semantics.

Validate the integration using the execution service described in
`EXECUTION.md` and record verified results, limitations, and the artifacts that
support them.

Write your answer to `REPORT.md` in this directory.
"""

END_TO_END_PROMPT = INTEGRATION_PROMPT.rstrip() + """

Then run a bounded campaign within the budget in `EXECUTION.md`, investigate
any finding it produces with the workspace commands in `CLI-README.md`, and
report what the evidence establishes.
"""

PANELS = {
    "investigation": Panel(
        name="investigation",
        needs_kvm=False,
        tools=["Bash", "Read", "Write", "Edit", "Glob", "Grep"],
        bash_allow=["harmony", "ls", "cat", "head", "tail", "grep", "wc",
                    "jq", "find", "sed", "awk"],
        prompt=INVESTIGATION_PROMPT,
        materials=["workspace", "CLI-README.md"],
    ),
    "integration": Panel(
        name="integration",
        needs_kvm=True,
        tools=["Bash", "Read", "Write", "Edit", "Glob", "Grep"],
        bash_allow=[],
        prompt=INTEGRATION_PROMPT,
        materials=["source", "CONTRACT.md", "EXECUTION.md", "SDK-README.md"],
    ),
    "end-to-end": Panel(
        name="end-to-end",
        needs_kvm=True,
        tools=["Bash", "Read", "Write", "Edit", "Glob", "Grep"],
        bash_allow=[],
        prompt=END_TO_END_PROMPT,
        materials=["source", "CONTRACT.md", "EXECUTION.md", "SDK-README.md",
                   "CLI-README.md"],
    ),
}

# Lines the fixture actually retained. A report that quotes one of these read
# the evidence; a report that describes it in general terms did not.
RETAINED = ["lacks matching index tuple"]


def digest_tree(root: Path) -> str:
    """A stable digest of every file under `root`."""
    hasher = hashlib.sha256()
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        hasher.update(str(path.relative_to(root)).encode())
        hasher.update(path.read_bytes())
    return hasher.hexdigest()


def _commands(transcript: list[dict]) -> list[str]:
    found = []
    for event in transcript:
        message = event.get("message")
        if not isinstance(message, dict):
            continue
        for item in message.get("content") or []:
            if isinstance(item, dict) and item.get("type") == "tool_use":
                command = (item.get("input") or {}).get("command")
                if isinstance(command, str):
                    found.append(command)
    return found


def _verbs(commands: list[str]) -> set[str]:
    verbs = set()
    for command in commands:
        for match in re.finditer(
            r"harmony\s+(?:-w|--workspace)\s+\S+\s+([a-z]+)", command
        ):
            verbs.add(match.group(1))
    return verbs


def grade_investigation(attempt_dir: Path, transcript: list[dict],
                        baseline: str) -> list[Check]:
    """Check one investigation attempt against its artifacts."""
    report_path = attempt_dir / "REPORT.md"
    report = report_path.read_text() if report_path.exists() else ""
    lowered = report.lower()
    commands = _commands(transcript)
    verbs = _verbs(commands)
    workspace = attempt_dir / "workspace"

    finding = json.dumps(_finding_record(workspace), sort_keys=True)
    refused_kvm = any("Linux KVM host" in json.dumps(event) for event in transcript)

    checks = [
        Check("submitted a report", bool(report.strip()),
              f"{len(report)} characters"),
        Check("used the workspace commands", len(verbs) >= 3,
              f"verbs used: {', '.join(sorted(verbs)) or 'none'}"),
        Check("named the finding", "bug-1" in lowered, "looked for bug-1"),
        Check("named the failed property", re.search(r"\b(assertion|property)\D{0,12}2\b",
                                                     lowered) is not None,
              "looked for property 2"),
        Check("named what the property claims",
              "index" in lowered and ("heap" in lowered or "tuple" in lowered),
              "looked for the declared meaning"),
        Check("named the command that evaluated it",
              "amcheck" in lowered or "hook 3" in lowered, "looked for hook 3"),
        Check("cited a retained id",
              re.search(r"\b(m-\d{4}|ev-\d{4})\b", report) is not None,
              "looked for a moment or evidence id"),
        Check("quoted retained evidence",
              any(line in report for line in RETAINED),
              "looked for a line the fixture retained"),
        Check("left the recorded finding unchanged",
              finding == baseline,
              "compared the finding record before and after"),
        Check("stated what it could not observe",
              any(word in lowered for word in
                  ("could not", "unable", "not observed", "unevaluated",
                   "no verdict", "limitation")),
              "looked for a stated limit", kind="claim"),
    ]
    if refused_kvm:
        checks.append(Check(
            "did not claim an advance the host refused",
            any(word in lowered for word in ("kvm", "could not advance",
                                             "linux", "not available")),
            "the CLI refused to advance on this host; the report must say so",
            kind="claim"))
    return checks


def _finding_record(workspace: Path) -> dict:
    journal = workspace / "journal"
    if not journal.is_dir():
        return {}
    for path in sorted(journal.glob("*.json")):
        for record in json.loads(path.read_text()).get("records", []):
            if record.get("record") == "finding":
                return record
    return {}


def baseline_finding(workspace: Path) -> str:
    return json.dumps(_finding_record(workspace), sort_keys=True)


GRADERS = {"investigation": grade_investigation}
