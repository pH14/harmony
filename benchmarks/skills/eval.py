#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The Harmony developer-skill evaluation runner.

One program owns attempt preparation, the model call, freezing the submission,
independent evaluation, and the report. GitHub Actions invokes it rather than
repeating any of that.

    ./benchmarks/skills/eval.py qualify --out runs/qualify
    ./benchmarks/skills/eval.py run --panel investigation --attempts 3 --out runs/pilot
    ./benchmarks/skills/eval.py report --out runs/pilot

An attempt gets a fresh directory holding only its own materials: the panel's
fixture, the factual references both arms receive, and — in the skills arm —
the four skills. The evaluator's ground truth, this repository, and other
attempts stay outside it.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import shutil
import subprocess
import sys
import time
from dataclasses import asdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import adapters  # noqa: E402
import fixture  # noqa: E402
import panels  # noqa: E402

FORMAT = "harmony-skill-evaluation-v1"
REPOSITORY = Path(__file__).resolve().parents[2]
ARMS = ("skills", "documentation")


def _digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _harmony(explicit: str | None) -> Path:
    """The CLI an attempt drives.

    Raises:
        SystemExit: when no built binary is available.
    """
    if explicit:
        binary = Path(explicit).resolve()
        if not binary.exists():
            raise SystemExit(f"{binary} does not exist")
        return binary
    for profile in ("release", "debug"):
        binary = REPOSITORY / "target" / profile / "harmony"
        if binary.exists():
            return binary
    raise SystemExit("no harmony binary; run `cargo build -p harmony-cli` "
                     "or pass --harmony PATH")


def _skills_digest() -> dict:
    """Every skill file and its hash, so a cohort can prove what it shipped."""
    root = REPOSITORY / "skills"
    return {str(path.relative_to(root)): _digest(path)
            for path in sorted(root.rglob("*.md"))}


def prepare(attempt_dir: Path, panel: panels.Panel, arm: str, harmony: Path,
            fixture_facts: dict) -> dict:
    """Lay out one attempt's isolated workspace."""
    if attempt_dir.exists():
        shutil.rmtree(attempt_dir)
    attempt_dir.mkdir(parents=True)
    supplied = []

    if "workspace" in panel.materials:
        source = Path(fixture_facts["workspace"])
        shutil.copytree(source, attempt_dir / "workspace")
        supplied.append("workspace/")
    if "CLI-README.md" in panel.materials:
        shutil.copy(REPOSITORY / "cli" / "README.md", attempt_dir / "CLI-README.md")
        supplied.append("CLI-README.md")
    if "SDK-README.md" in panel.materials:
        shutil.copy(REPOSITORY / "consonance" / "harmony-linux" / "sdk" / "README.md",
                    attempt_dir / "SDK-README.md")
        supplied.append("SDK-README.md")

    # Both arms drive the same binary through the same path.
    tools = attempt_dir / "bin"
    tools.mkdir()
    (tools / "harmony").symlink_to(harmony)
    supplied.append("bin/harmony")

    if arm == "skills":
        destination = attempt_dir / ".claude" / "skills"
        destination.parent.mkdir(exist_ok=True)
        shutil.copytree(REPOSITORY / "skills", destination)
        supplied.append(".claude/skills/")

    allow = [f"Bash({name}:*)" for name in panel.bash_allow]
    settings = {"permissions": {"allow": allow, "deny": [], "defaultMode": "default"}}
    (attempt_dir / ".claude" / "settings.json").parent.mkdir(exist_ok=True)
    (attempt_dir / ".claude" / "settings.json").write_text(json.dumps(settings, indent=1))
    return {
        "supplied": supplied,
        "bash_allow": panel.bash_allow,
        # The contract asks for a pinned container per attempt. A directory on
        # the runner host is weaker: the attempt's file tools are confined to
        # it, and its commands are the allowlist above, but it shares the host
        # and the operator's agent configuration. Reports say which was used.
        "isolation": "directory on the runner host",
    }


def launch(adapter, attempt_dir: Path, panel: panels.Panel,
           budget: adapters.Budget) -> adapters.Attempt:
    environment = dict(os.environ)
    environment["PATH"] = f"{attempt_dir / 'bin'}{os.pathsep}{environment['PATH']}"
    previous = os.environ.get("PATH")
    os.environ["PATH"] = environment["PATH"]
    try:
        return adapter.run(attempt_dir, panel.prompt, budget)
    finally:
        if previous is not None:
            os.environ["PATH"] = previous


def freeze(attempt_dir: Path, into: Path) -> dict:
    """Copy the submission out of the attempt directory and digest it."""
    frozen = into / "submission"
    if frozen.exists():
        shutil.rmtree(frozen)
    shutil.copytree(attempt_dir, frozen, symlinks=True,
                    ignore=shutil.ignore_patterns(".claude-config"))
    return {"digest": panels.digest_tree(frozen), "path": str(frozen)}


def one_attempt(out: Path, panel: panels.Panel, arm: str, index: int,
                adapter, budget: adapters.Budget, harmony: Path,
                fixture_facts: dict) -> dict:
    label = f"{panel.name}-{arm}-{index}"
    record_dir = out / label
    record_dir.mkdir(parents=True, exist_ok=True)
    attempt_dir = record_dir / "attempt"
    supplied = prepare(attempt_dir, panel, arm, harmony, fixture_facts)
    baseline = (panels.baseline_finding(attempt_dir / "workspace")
                if (attempt_dir / "workspace").exists() else "")

    started = time.time()
    result = launch(adapter, attempt_dir, panel, budget)
    frozen = freeze(attempt_dir, record_dir)

    (record_dir / "transcript.jsonl").write_text(
        "".join(json.dumps(event) + "\n" for event in result.transcript))

    grader = panels.GRADERS.get(panel.name)
    if grader is None:
        checks: list[panels.Check] = []
        verdict = "ungraded"
    elif result.infrastructure_error:
        checks = []
        verdict = "infrastructure"
    else:
        checks = grader(Path(frozen["path"]), result.transcript, baseline)
        artifact = [check for check in checks if check.kind == "artifact"]
        verdict = "pass" if all(check.passed for check in artifact) else "fail"

    record = {
        "format": FORMAT,
        "attempt": label,
        "panel": panel.name,
        "arm": arm,
        "index": index,
        "verdict": verdict,
        "stopped_by": result.stopped_by,
        "exit_status": result.exit_status,
        "infrastructure_error": result.infrastructure_error,
        "seconds": round(result.seconds, 1),
        "tool_calls": result.tool_calls,
        "usage": result.usage,
        "adapter": adapter.name,
        "model": result.model,
        "adapter_version": result.adapter_version,
        "budget": asdict(budget),
        "supplied": supplied,
        "fixture": fixture_facts,
        "skills": _skills_digest() if arm == "skills" else {},
        "submission": frozen,
        "started": started,
        "checks": [asdict(check) for check in checks],
    }
    (record_dir / "attempt.json").write_text(json.dumps(record, indent=1))
    return record


def command_run(args: argparse.Namespace) -> int:
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    harmony = _harmony(args.harmony)
    panel = panels.PANELS[args.panel]

    fixture_root = out / "fixture"
    if args.recorded_workspace:
        facts = fixture.adopt(Path(args.recorded_workspace), fixture_root)
    elif "workspace" in panel.materials:
        facts = fixture.derive(fixture_root, REPOSITORY)
    else:
        facts = {"source": "none"}
    (out / "fixture.json").write_text(json.dumps(facts, indent=1))

    adapter = adapters.build(args.adapter, model=args.model, effort=args.effort)
    budget = adapters.Budget(wall_seconds=args.wall_seconds,
                             total_tokens=args.total_tokens,
                             tool_calls=args.tool_calls)

    # Matched pairs in a randomized order, so a systematic drift in provider
    # behavior over the session cannot land on one arm.
    order = [(arm, index) for index in range(1, args.attempts + 1) for arm in ARMS]
    random.Random(args.seed).shuffle(order)

    records = []
    for arm, index in order:
        print(f"[{panel.name}] {arm} attempt {index}", file=sys.stderr, flush=True)
        record = one_attempt(out, panel, arm, index, adapter, budget, harmony, facts)
        print(f"    {record['verdict']} ({record['stopped_by']}, "
              f"{record['seconds']}s)", file=sys.stderr, flush=True)
        records.append(record)
    _write_report(out, records)
    return 0


def command_qualify(args: argparse.Namespace) -> int:
    """Check the runner and the fixture without calling a model."""
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    harmony = _harmony(args.harmony)
    facts = fixture.derive(out / "fixture", REPOSITORY)

    results = []

    def check(name: str, ok: bool, evidence: str) -> None:
        results.append({"check": name, "passed": ok, "evidence": evidence})

    workspace = Path(facts["workspace"])
    listed = subprocess.run([harmony, "-w", workspace, "--json", "findings"],
                            capture_output=True, text=True, check=False)
    check("the fixture opens as a workspace", listed.returncode == 0,
          listed.stderr.strip()[:400] or "findings listed")
    findings = json.loads(listed.stdout)["findings"] if listed.returncode == 0 else []
    check("it holds exactly one finding", len(findings) == 1, f"{len(findings)} findings")
    check("the failed property carries its meaning",
          bool(findings) and "matching entry" in findings[0]["meanings"][0],
          findings[0]["meanings"][0] if findings else "no finding")

    described = subprocess.run([harmony, "-w", workspace, "--json", "inspect", "bug-1"],
                               capture_output=True, text=True, check=False)
    summary = json.loads(described.stdout) if described.returncode == 0 else {}
    check("inspection names the hook that reported it",
          summary.get("properties", [{}])[0].get("reported_by_hook") == 3,
          json.dumps(summary.get("properties", [])[:1]))
    check("a silent check is reported unevaluated rather than satisfied",
          sorted(summary.get("unevaluated", [])) == [26, 27],
          f"unevaluated: {summary.get('unevaluated')}")

    evidence = subprocess.run(
        [harmony, "-w", workspace, "--json", "inspect", "bug-1", "command"],
        capture_output=True, text=True, check=False)
    lines = json.loads(evidence.stdout)["lines"] if evidence.returncode == 0 else []
    check("the retained detector output is readable",
          any("lacks matching index tuple" in line for line in lines),
          f"{len(lines)} lines")

    missing = subprocess.run([harmony, "-w", workspace, "inspect", "bug-9"],
                             capture_output=True, text=True, check=False)
    check("an unknown selector is refused rather than substituted",
          missing.returncode != 0 and "bug-9" in missing.stderr,
          missing.stderr.strip()[:200])

    adapter = adapters.build(args.adapter, model=args.model, effort=args.effort)
    version = adapter.version()
    check("the agent adapter is available", "unavailable" not in version, version)

    passed = all(item["passed"] for item in results)
    report = {"format": FORMAT, "stage": "qualify", "passed": passed,
              "fixture": facts, "checks": results,
              "harmony": str(harmony), "adapter": adapter.name,
              "adapter_version": version, "model": adapter.model}
    (out / "qualify.json").write_text(json.dumps(report, indent=1))
    for item in results:
        print(f"{'ok  ' if item['passed'] else 'FAIL'} {item['check']}: {item['evidence']}")
    return 0 if passed else 1


def _write_report(out: Path, records: list[dict]) -> None:
    by_arm: dict[str, list[dict]] = {arm: [] for arm in ARMS}
    for record in records:
        by_arm[record["arm"]].append(record)
    summary = {
        "format": FORMAT,
        "panel": records[0]["panel"] if records else None,
        "model": records[0]["model"] if records else None,
        "adapter_version": records[0]["adapter_version"] if records else None,
        "fixture": records[0]["fixture"] if records else None,
        "isolation": records[0]["supplied"].get("isolation") if records else None,
        "arms": {
            arm: {
                "attempts": len(items),
                "passed": sum(1 for item in items if item["verdict"] == "pass"),
                "failed": sum(1 for item in items if item["verdict"] == "fail"),
                "infrastructure": sum(1 for item in items
                                      if item["verdict"] == "infrastructure"),
                "checks": _check_rates(items),
            }
            for arm, items in by_arm.items()
        },
        "attempts": [
            {k: record[k] for k in
             ("attempt", "arm", "verdict", "stopped_by", "seconds", "tool_calls")}
            for record in records
        ],
    }
    (out / "report.json").write_text(json.dumps(summary, indent=1))
    (out / "report.md").write_text(_markdown(summary))


def _check_rates(items: list[dict]) -> dict:
    rates: dict[str, list[int]] = {}
    for item in items:
        for check in item["checks"]:
            rates.setdefault(check["name"], []).append(1 if check["passed"] else 0)
    return {name: f"{sum(values)}/{len(values)}" for name, values in rates.items()}


def _markdown(summary: dict) -> str:
    lines = [f"# {summary['panel']} panel", "",
             f"Model: {summary['model']} via {summary['adapter_version']}", ""]
    source = (summary.get("fixture") or {}).get("source")
    if source == "derived":
        lines += ["The fixture was derived from the historical case without "
                  "executing a VM, so this panel measures CLI usability and "
                  "not a real investigation.", ""]
    if summary.get("isolation"):
        lines += [f"Isolation: {summary['isolation']}.", ""]
    lines += ["| Arm | Attempts | Passed | Failed | Infrastructure |",
              "| --- | --- | --- | --- | --- |"]
    for arm, facts in summary["arms"].items():
        lines.append(f"| {arm} | {facts['attempts']} | {facts['passed']} | "
                     f"{facts['failed']} | {facts['infrastructure']} |")
    lines += ["", "## Checks", ""]
    names = sorted({name for facts in summary["arms"].values()
                    for name in facts["checks"]})
    lines += ["| Check | " + " | ".join(summary["arms"]) + " |",
              "| --- | " + " | ".join("---" for _ in summary["arms"]) + " |"]
    for name in names:
        row = [summary["arms"][arm]["checks"].get(name, "-") for arm in summary["arms"]]
        lines.append(f"| {name} | " + " | ".join(row) + " |")
    lines += ["", "## Attempts", "",
              "| Attempt | Arm | Verdict | Stopped by | Seconds | Tool calls |",
              "| --- | --- | --- | --- | --- | --- |"]
    for attempt in summary["attempts"]:
        lines.append("| " + " | ".join(str(attempt[k]) for k in
                     ("attempt", "arm", "verdict", "stopped_by", "seconds",
                      "tool_calls")) + " |")
    return "\n".join(lines) + "\n"


def command_report(args: argparse.Namespace) -> int:
    out = Path(args.out).resolve()
    records = [json.loads(path.read_text())
               for path in sorted(out.glob("*/attempt.json"))]
    if not records:
        raise SystemExit(f"no attempts under {out}")
    _write_report(out, records)
    print((out / "report.md").read_text())
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    def shared(target: argparse.ArgumentParser) -> None:
        target.add_argument("--out", required=True)
        target.add_argument("--harmony")
        target.add_argument("--adapter", default="claude-code",
                            choices=sorted(adapters.ADAPTERS))
        target.add_argument("--model", default="claude-sonnet-5")
        target.add_argument("--effort", default="medium")

    qualify = sub.add_parser("qualify", help="check the runner and fixture")
    shared(qualify)
    qualify.set_defaults(handler=command_qualify)

    run = sub.add_parser("run", help="run one panel's matched attempts")
    shared(run)
    run.add_argument("--panel", required=True, choices=sorted(panels.PANELS))
    run.add_argument("--attempts", type=int, default=3)
    run.add_argument("--seed", type=int, default=7)
    run.add_argument("--wall-seconds", type=int, default=1800)
    run.add_argument("--total-tokens", type=int, default=2_000_000)
    run.add_argument("--tool-calls", type=int, default=400)
    run.add_argument("--recorded-workspace",
                     help="a workspace a real search wrote, instead of a "
                          "derived fixture")
    run.set_defaults(handler=command_run)

    report = sub.add_parser("report", help="rebuild the report from attempts")
    report.add_argument("--out", required=True)
    report.set_defaults(handler=command_report)

    args = parser.parse_args(argv)
    return args.handler(args)


if __name__ == "__main__":
    raise SystemExit(main())
