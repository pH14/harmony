#!/usr/bin/env python3
"""Score historical discovery campaigns and render the arm comparison.

Each campaign directory holds one search (``<case>.search``) and, when the
search confirmed a finding carrying the case's scored assertion or one of its
declared integrity assertions, the fresh replays of the first such finding
(``<case>.reproduce-*``). Every campaign gets exactly
one outcome; the rules are in workloads/bugs/historical/DISCOVERY.md.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUTCOMES = (
    "discovery",
    "replay-failure",
    "unconfirmed",
    "internal-discovery",
    "other-violation",
    "guest-crash",
    "miss",
    "inconclusive",
    "infra-failure",
)
ARM_ORDER = {"guided": 0, "general": 1, "ablation": 2, "heldout": 3}


@dataclass
class Campaign:
    case_id: str
    seed: int | None
    outcome: str
    executions: int = 0
    wall_seconds: float = 0.0
    executions_to_first: int | None = None
    seconds_to_first: float | None = None
    evidence_reached: bool = False
    crashes: int = 0
    other_violations: list[str] = field(default_factory=list)
    assertion: str = ""
    detail: str = ""


def load_json(path: Path) -> dict | None:
    try:
        return json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None


def seconds_at(progress: Path, execution: int, fallback: float) -> float:
    """Wall seconds when the search had completed `execution` executions."""
    try:
        lines = progress.read_text().splitlines()
    except OSError:
        return fallback
    for line in lines:
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if record.get("executions", 0) >= execution:
            return record.get("search_elapsed_millis", 0) / 1000
    return fallback


NODE_EXIT = "workload node ends only by a fault the search injected"


def internal_assertion(bugs: list[dict], case: dict) -> bool:
    functions = case["oracle"].get("fix_functions") or []
    for bug in bugs:
        replay = bug.get("replay") or {}
        if not (bug.get("confirmed") and NODE_EXIT in (bug.get("violations") or [])):
            continue
        consoles = [step.get("console") or "" for step in replay.get("timeline") or []]
        for line in "\n".join(consoles).splitlines():
            if "Assertion `" in line and any(f" {name}(" in line for name in functions):
                return True
    return False


def integrity_checks(case: dict) -> list[tuple[str, str]]:
    oracle = case["oracle"]
    return [(oracle["assertion"], oracle["evidence"])] + [
        (check["assertion"], check["evidence"]) for check in oracle.get("integrity") or []
    ]


def carried(bug: dict, checks: list[tuple[str, str]]) -> tuple[str, str] | None:
    violations = bug.get("violations") or []
    return next((check for check in checks if check[0] in violations), None)


def score(search: Path, case: dict) -> Campaign:
    evidence = case["oracle"]["evidence"]
    checks = integrity_checks(case)
    status = load_json(search / "panel-status.json") or {}
    report = load_json(search / "report.json")
    summary = load_json(search / "campaign-summary.json") or {}
    campaign = Campaign(case_id=case["id"], seed=status.get("seed"), outcome="infra-failure")
    if report is None:
        campaign.detail = "no report"
        return campaign
    campaign.executions = int(report.get("executions") or 0)
    campaign.wall_seconds = float(report.get("wall_seconds") or 0)
    if status.get("execution_status") == "infra_failure" or str(status.get("oracle", "")).startswith(
        "fail: infra-failure"
    ):
        campaign.detail = str(status.get("oracle", "infra-failure"))
        return campaign
    campaign.evidence_reached = bool(summary.get("assertions", {}).get(evidence, {}).get("passed"))
    bugs = report.get("bugs") or []
    scored = [bug for bug in bugs if carried(bug, checks)]
    confirmed = [
        bug
        for bug in scored
        if bug.get("confirmed")
        and (check := carried(bug, checks))
        and check[1] in (bug.get("sometimes") or [])
        and (bug.get("replay") or {}).get("bug")
        and check[0] in ((bug.get("replay") or {}).get("violations") or [])
    ]
    first = min(confirmed, key=lambda bug: bug["execution"]) if confirmed else None
    if first is not None:
        campaign.assertion = carried(first, checks)[0]
    for bug in bugs:
        if not bug.get("confirmed"):
            continue
        violations = bug.get("violations") or []
        if not violations and (bug.get("replay") or {}).get("stop") == "Crash":
            campaign.crashes += 1
        for violation in violations:
            if violation != campaign.assertion and violation not in campaign.other_violations:
                campaign.other_violations.append(violation)
    if first is not None:
        campaign.executions_to_first = int(first["execution"])
        campaign.seconds_to_first = seconds_at(
            search / "progress.jsonl", campaign.executions_to_first, campaign.wall_seconds
        )
        replays = [
            load_json(path / "panel-status.json") or {}
            for path in sorted(search.parent.glob(f"{case['id']}.reproduce-*"))
            if path.is_dir()
        ]
        if any(replay.get("oracle") != "pass" for replay in replays) or not replays:
            campaign.outcome = "replay-failure"
            campaign.detail = "; ".join(str(replay.get("oracle", "no replay")) for replay in replays) or "no fresh replay"
        else:
            campaign.outcome = "discovery"
        return campaign
    if scored:
        campaign.outcome = "unconfirmed"
    elif internal_assertion(bugs, case):
        campaign.outcome = "internal-discovery"
    elif campaign.other_violations:
        campaign.outcome = "other-violation"
    elif campaign.crashes:
        campaign.outcome = "guest-crash"
    elif campaign.evidence_reached:
        campaign.outcome = "miss"
    else:
        campaign.outcome = "inconclusive"
    return campaign


def cases() -> dict[str, dict]:
    found = {}
    for path in sorted((ROOT / "workloads/bugs/historical").glob("*/case.json")):
        case = json.loads(path.read_text())
        found[case["id"]] = case
    return found


def collect(reports: Path, known: dict[str, dict]) -> list[Campaign]:
    campaigns = []
    for search in sorted(reports.rglob("*.search")):
        if not search.is_dir():
            continue
        case_id = search.name[: -len(".search")]
        if case_id in known:
            campaigns.append(score(search, known[case_id]))
    return campaigns


def fisher_two_sided(a: int, b: int, c: int, d: int) -> float:
    """Two-sided Fisher exact test for the table [[a, b], [c, d]]."""
    rows, column, total = a + b, a + c, a + b + c + d

    def probability(x: int) -> float:
        return math.comb(column, x) * math.comb(total - column, rows - x) / math.comb(total, rows)

    observed = probability(a)
    low, high = max(0, rows + column - total), min(rows, column)
    return min(1.0, sum(p for x in range(low, high + 1) if (p := probability(x)) <= observed * (1 + 1e-9)))


def median(values: list[float]) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    middle = len(ordered) // 2
    return ordered[middle] if len(ordered) % 2 else (ordered[middle - 1] + ordered[middle]) / 2


def censored_median(campaign_values: list[float | None], budget: float) -> str:
    """Median over every campaign, a miss counting as above the budget."""
    if not campaign_values:
        return "—"
    values = [value if value is not None else math.inf for value in campaign_values]
    middle = median(values)
    if middle is None or math.isinf(middle):
        return f"> {budget:g}"
    return f"{middle:.0f}"


def render(campaigns: list[Campaign], known: dict[str, dict]) -> str:
    groups: dict[str, list[str]] = {}
    for case_id in sorted({campaign.case_id for campaign in campaigns}):
        groups.setdefault(known[case_id].get("focused_case", case_id), []).append(case_id)
    lines = []
    for focused, members in sorted(groups.items()):
        members.sort(key=lambda case_id: (ARM_ORDER.get(known[case_id].get("discovery_mode", "guided"), 9), case_id))
        lines += [
            f"### {known[focused]['title'] if focused in known else focused}",
            "",
            "| arm | campaigns | discoveries | median s to first | median executions to first | "
            + " | ".join(outcome for outcome in OUTCOMES[1:])
            + " | evidence reached | executions/s | Fisher p vs focused |",
            "|---|---:|---:|---:|---:|" + "---:|" * (len(OUTCOMES) - 1) + "---:|---:|---:|",
        ]
        reference = [campaign for campaign in campaigns if campaign.case_id == focused]
        reference_hits = sum(campaign.outcome == "discovery" for campaign in reference)
        for case_id in members:
            arm = [campaign for campaign in campaigns if campaign.case_id == case_id]
            hits = sum(campaign.outcome == "discovery" for campaign in arm)
            budget_s = max((campaign.wall_seconds for campaign in arm), default=0)
            budget_x = max((campaign.executions for campaign in arm), default=0)
            seconds = [campaign.seconds_to_first if campaign.outcome == "discovery" else None for campaign in arm]
            executions = [
                float(campaign.executions_to_first) if campaign.outcome == "discovery" else None for campaign in arm
            ]
            rate = sum(campaign.executions for campaign in arm) / max(sum(campaign.wall_seconds for campaign in arm), 1)
            if case_id == focused or not reference:
                p_value = "—"
            else:
                p_value = f"{fisher_two_sided(reference_hits, len(reference) - reference_hits, hits, len(arm) - hits):.3g}"
            counts = " | ".join(str(sum(campaign.outcome == outcome for campaign in arm)) for outcome in OUTCOMES[1:])
            mode = known[case_id].get("discovery_mode", "guided")
            lines.append(
                f"| `{case_id}` ({'focused' if mode == 'guided' else mode}) | {len(arm)} | {hits} | "
                f"{censored_median(seconds, budget_s)} | {censored_median(executions, budget_x)} | {counts} | "
                f"{sum(campaign.evidence_reached for campaign in arm)} | {rate:.1f} | {p_value} |"
            )
        lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reports", type=Path, required=True)
    parser.add_argument("--json", type=Path, help="write every campaign's outcome here")
    args = parser.parse_args()
    known = cases()
    campaigns = collect(args.reports, known)
    if args.json:
        args.json.write_text(json.dumps([asdict(campaign) for campaign in campaigns], indent=2) + "\n")
    sys.stdout.write(render(campaigns, known) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
