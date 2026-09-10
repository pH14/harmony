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
    # Both arms of a panel run under the panel's own ceilings. The token
    # ceiling counts cache reads, so it scales with the number of turns a
    # panel's work takes rather than with its difficulty.
    wall_seconds: int = 1_800
    total_tokens: int = 2_000_000
    tool_calls: int = 400


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
Prepare the PostgreSQL release in `source/` for testing in Harmony.

Focus on the correctness of `CREATE INDEX CONCURRENTLY` under concurrent
updates and maintenance. Read `CONTRACT.md` for the behavior that feature is
supposed to have, `WORKLOAD-README.md` and `SDK-README.md` for how a workload
declares and reports itself, and `EXECUTION.md` for how to build and run one.

Identify and prioritize the properties worth checking. Implement checks and
observations for them, and produce a reproducible build and a workload image.
Preserve PostgreSQL's semantics: do not patch the source to make a check pass.
Use compiler or runtime instrumentation only where the references say it
changes what Harmony explores.

Leave your work in this directory:

* `bundle` — the workload bundle the image carries, declaring its nodes, hooks,
  properties, and diagnostics.
* `Dockerfile` and anything it needs, pinning the source by checksum.
* `REPORT.md`.

`REPORT.md` must state the properties you chose and why, which check evaluates
each one and what its silence means, what you ran and what the results were,
and what the integration does not cover.

Check your bundle with `harmony preflight --bundle bundle` before you finish.
Report what you verified and what you did not. Do not claim a result you did
not run.
"""

END_TO_END_PROMPT = INTEGRATION_PROMPT.rstrip() + """

Then run one bounded campaign within the budget in `EXECUTION.md`, writing its
workspace to `campaign/`. If it produces a finding, investigate it with the
workspace commands in `CLI-README.md` and cite the moment and evidence ids the
CLI returned.

A campaign that finds nothing is not proof the workload is correct. Say what
ran, what stopped it, and what it did not reach.
"""

# Reading and building are the same everywhere; a preparation panel adds the
# tools that turn source into an image and the CLI that runs it.
READING = ["ls", "cat", "head", "tail", "grep", "wc", "jq", "find", "sed", "awk"]
BUILDING = ["tar", "bzip2", "docker", "make", "sh", "cp", "mv", "mkdir",
            "chmod", "sha256sum", "shasum", "printf", "echo", "test", "true"]

PANELS = {
    "investigation": Panel(
        name="investigation",
        needs_kvm=False,
        tools=["Bash", "Read", "Write", "Edit", "Glob", "Grep"],
        bash_allow=["harmony", *READING],
        prompt=INVESTIGATION_PROMPT,
        materials=["workspace", "CLI-README.md"],
    ),
    "integration": Panel(
        name="integration",
        needs_kvm=True,
        tools=["Bash", "Read", "Write", "Edit", "Glob", "Grep"],
        bash_allow=["harmony", *READING, *BUILDING],
        prompt=INTEGRATION_PROMPT,
        materials=["source", "CONTRACT.md", "EXECUTION.md", "SDK-README.md",
                   "WORKLOAD-README.md"],
        wall_seconds=5_400,
        total_tokens=16_000_000,
        tool_calls=800,
    ),
    "end-to-end": Panel(
        name="end-to-end",
        needs_kvm=True,
        tools=["Bash", "Read", "Write", "Edit", "Glob", "Grep"],
        bash_allow=["harmony", *READING, *BUILDING],
        prompt=END_TO_END_PROMPT,
        materials=["source", "CONTRACT.md", "EXECUTION.md", "SDK-README.md",
                   "WORKLOAD-README.md", "CLI-README.md"],
        wall_seconds=9_000,
        total_tokens=24_000_000,
        tool_calls=1_200,
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
                        baseline: str, harmony: Path) -> list[Check]:
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


def source_baseline(source: Path) -> str:
    """What a preparation attempt must leave as it found it."""
    return _source_digest(source)


def baseline_finding(workspace: Path) -> str:
    return json.dumps(_finding_record(workspace), sort_keys=True)


def _bundle_report(harmony: Path, bundle: Path) -> dict:
    """What `harmony preflight --bundle` says about a submitted bundle.

    Grading reads the bundle through the shipping parser rather than a copy of
    the grammar, so a bundle that grades well is one the product accepts.
    """
    import subprocess

    result = subprocess.run(
        [str(harmony), "--json", "preflight", "--bundle", str(bundle)],
        capture_output=True, text=True, check=False)
    try:
        return json.loads(result.stdout).get("bundle") or {}
    except json.JSONDecodeError:
        return {"error": result.stderr.strip() or "preflight produced no report"}


def _programs(bundle: Path) -> set[str]:
    """Every guest program a bundle names, by the path it names."""
    found = set()
    for line in (bundle.read_text().splitlines() if bundle.exists() else []):
        words = line.split()
        if not words:
            continue
        if words[0] in ("setup", "ready") and len(words) > 1:
            found.add(words[1])
        elif words[0] in ("node", "hook") and len(words) > 2:
            found.add(words[2])
    return found


def _missing_programs(attempt_dir: Path, recipe: str, named: set[str]) -> list[str]:
    """The programs a bundle names that the submission neither writes nor builds."""
    written = {path.name for path in attempt_dir.rglob("*")
               if path.is_file() and (attempt_dir / "source") not in path.parents}
    return sorted(argv for argv in named
                  if Path(argv).name not in written and Path(argv).name not in recipe)


def _claims_coverage_guidance(report: str) -> bool:
    """Whether a report says the guest's instrumentation guides the search.

    A sentence that mentions the library or coverage guidance and denies it is
    the accurate statement, so only an undenied mention counts as a claim.
    """
    denials = ("not", "n't", " no ", "without", "unused", "out of scope",
               "unqualified", "absent")
    for sentence in re.split(r"(?<=[.!?\n])\s+", report.lower()):
        if "libvoidstar" not in sentence and "coverage-guided" not in sentence:
            continue
        if not any(word in sentence for word in denials):
            return True
    return False


def _source_digest(root: Path) -> str:
    """The digest of the supplied release, ignoring anything unpacked beside it."""
    tarballs = sorted(root.glob("*.tar.bz2"))
    return digest_tree(root) if not tarballs else hashlib.sha256(
        b"".join(p.read_bytes() for p in tarballs)).hexdigest()


def grade_integration(attempt_dir: Path, transcript: list[dict],
                      baseline: str, harmony: Path) -> list[Check]:
    """Check one preparation attempt against the artifacts it left behind."""
    report_path = attempt_dir / "REPORT.md"
    report = report_path.read_text() if report_path.exists() else ""
    lowered = report.lower()
    bundle_path = attempt_dir / "bundle"
    declared = _bundle_report(harmony, bundle_path) if bundle_path.exists() else {}
    assertions = declared.get("assertions") or []
    recipe = "\n".join(
        path.read_text(errors="replace")
        for path in sorted(attempt_dir.glob("Dockerfile*")))
    named = _programs(bundle_path)
    missing = _missing_programs(attempt_dir, recipe, named)

    checks = [
        Check("submitted a report", bool(report.strip()),
              f"{len(report)} characters"),
        Check("produced a bundle", bundle_path.exists(),
              f"looked for {bundle_path.name}"),
        Check("the bundle is one the product accepts",
              bool(declared) and declared.get("error") is None
              and not declared.get("blockers"),
              declared.get("error") or "; ".join(declared.get("blockers") or [])
              or ("no bundle" if not declared else "no blockers")),
        Check("declared a property that can fail",
              any(a.startswith("always ") for a in assertions),
              f"{len(assertions)} declared: "
              + "; ".join(a.split(":")[0] for a in assertions)),
        Check("declared a way to read evidence",
              bool(declared.get("diagnostics")),
              f"diagnostics: {', '.join(declared.get('diagnostics') or []) or 'none'}"),
        Check("pinned the source by checksum",
              re.search(r"\b[0-9a-f]{64}\b", recipe) is not None,
              f"{len(recipe)} characters of build recipe"),
        Check("built for the guest architecture",
              "amd64" in recipe or "x86_64" in recipe,
              "looked for an explicit linux/amd64 build"),
        Check("left the supplied release unmodified",
              _source_digest(attempt_dir / "source") == baseline,
              "compared the release tarball before and after"),
        Check("wrote the programs the bundle names",
              bool(named) and not missing,
              f"bundle names {', '.join(sorted(named)) or 'nothing'}"
              + (f"; missing {', '.join(missing)}" if missing else "")),
        Check("did not claim coverage-guided exploration",
              not _claims_coverage_guidance(report),
              "the fault search consumes no basic-block identities",
              kind="claim"),
        Check("said what the integration does not cover",
              any(word in lowered for word in
                  ("does not cover", "not covered", "limitation", "did not run",
                   "could not", "unverified", "untested")),
              "looked for a stated limit", kind="claim"),
    ]
    return checks


def grade_end_to_end(attempt_dir: Path, transcript: list[dict],
                     baseline: str, harmony: Path) -> list[Check]:
    """The preparation checks, plus what the campaign and its workspace show."""
    checks = grade_integration(attempt_dir, transcript, baseline, harmony)
    report_path = attempt_dir / "REPORT.md"
    report = report_path.read_text() if report_path.exists() else ""
    lowered = report.lower()
    campaign = attempt_dir / "campaign"
    commands = _commands(transcript)
    searched = any("harmony" in c and "search" in c for c in commands)
    findings = _finding_record(campaign)

    checks += [
        Check("ran one campaign", searched and (campaign / "journal").is_dir(),
              f"{len(commands)} commands; workspace "
              + ("present" if (campaign / "journal").is_dir() else "absent")),
        Check("stayed inside the campaign budget",
              sum(1 for c in commands if "harmony" in c and "search" in c) <= 1,
              "one campaign is the budget"),
        Check("investigated the finding it produced" if findings
              else "did not claim a finding it does not have",
              bool(_verbs(commands) & {"inspect", "findings"}) if findings
              else "bug-1" not in lowered,
              "the campaign recorded a finding" if findings
              else "the campaign recorded no finding"),
        Check("did not read a bounded campaign as proof of correctness",
              bool(findings) or any(
                  word in lowered for word in
                  ("not proof", "does not prove", "no finding", "found nothing",
                   "cannot conclude")),
              "a campaign with no finding establishes nothing about correctness",
              kind="claim"),
    ]
    return checks


GRADERS = {
    "investigation": grade_investigation,
    "integration": grade_integration,
    "end-to-end": grade_end_to_end,
}
