#!/usr/bin/env python3
"""Tests for the historical discovery scorecard."""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("historical_discovery", HERE / "historical-discovery.py")
discovery = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = discovery
SPEC.loader.exec_module(discovery)

CASE = {
    "id": "toy",
    "title": "Toy",
    "focused_case": "toy",
    "discovery_mode": "guided",
    "oracle": {"assertion": "toy holds", "evidence": "toy checked"},
}
GENERAL = {**CASE, "id": "toy-general", "focused_case": "toy", "discovery_mode": "general"}


def bug(execution, violations, *, confirmed=True, evidence=True, replay_stop="Deadline"):
    return {
        "execution": execution,
        "confirmed": confirmed,
        "violations": violations,
        "sometimes": ["toy checked"] if evidence else [],
        "replay": {"bug": True, "violations": violations, "stop": replay_stop},
    }


class Scoring(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = Path(self.dir.name)

    def tearDown(self):
        self.dir.cleanup()

    def campaign(self, name, *, bugs=(), evidence=True, status=None, replays=(), case_id="toy", report=True):
        search = self.root / name / f"{case_id}.search"
        search.mkdir(parents=True)
        (search / "panel-status.json").write_text(
            json.dumps(status or {"seed": 7, "execution_status": "complete", "oracle": "pass"})
        )
        if report:
            (search / "report.json").write_text(
                json.dumps({"executions": 100, "wall_seconds": 60, "bugs": list(bugs)})
            )
        (search / "campaign-summary.json").write_text(
            json.dumps({"assertions": {"toy checked": {"passed": evidence}}})
        )
        (search / "progress.jsonl").write_text(
            "\n".join(
                json.dumps({"executions": e, "search_elapsed_millis": e * 500}) for e in (10, 40, 80, 100)
            )
            + "\n"
        )
        for index, verdict in enumerate(replays):
            replay = self.root / name / f"{case_id}.reproduce-{index}"
            replay.mkdir()
            (replay / "panel-status.json").write_text(json.dumps({"oracle": verdict}))
        return search

    def outcome(self, search, case=CASE):
        return discovery.score(search, case)

    def test_a_confirmed_scored_finding_with_fresh_replays_is_a_discovery(self):
        result = self.outcome(self.campaign("a", bugs=[bug(40, ["toy holds"])], replays=["pass"]))
        self.assertEqual(result.outcome, "discovery")
        self.assertEqual(result.executions_to_first, 40)
        self.assertEqual(result.seconds_to_first, 20.0)

    def test_replay_console_and_denial_files_are_not_replays(self):
        search = self.campaign("a", bugs=[bug(40, ["toy holds"])], replays=["pass"])
        for suffix in ("console.txt", "denial.json"):
            (search.parent / f"toy.reproduce-0.{suffix}").write_text("{}")
        self.assertEqual(self.outcome(search).outcome, "discovery")

    def test_a_failed_fresh_replay_is_not_a_discovery(self):
        result = self.outcome(self.campaign("a", bugs=[bug(40, ["toy holds"])], replays=["fail: replay-mismatch"]))
        self.assertEqual(result.outcome, "replay-failure")
        result = self.outcome(self.campaign("b", bugs=[bug(40, ["toy holds"])]))
        self.assertEqual(result.outcome, "replay-failure")

    def test_a_scored_finding_without_evidence_or_confirmation_is_unconfirmed(self):
        result = self.outcome(self.campaign("a", bugs=[bug(40, ["toy holds"], evidence=False)]))
        self.assertEqual(result.outcome, "unconfirmed")
        result = self.outcome(self.campaign("b", bugs=[bug(40, ["toy holds"], confirmed=False)]))
        self.assertEqual(result.outcome, "unconfirmed")

    def test_a_guest_crash_is_never_a_discovery(self):
        result = self.outcome(self.campaign("a", bugs=[bug(40, [], replay_stop="Crash")]))
        self.assertEqual(result.outcome, "guest-crash")
        self.assertEqual(result.crashes, 1)
        result = self.outcome(
            self.campaign("b", bugs=[bug(30, [], replay_stop="Crash"), bug(40, ["toy holds"])], replays=["pass"])
        )
        self.assertEqual(result.outcome, "discovery")
        self.assertEqual(result.crashes, 1)

    def test_another_assertion_is_reported_but_not_scored(self):
        result = self.outcome(self.campaign("a", bugs=[bug(40, ["something else"])]))
        self.assertEqual(result.outcome, "other-violation")
        self.assertEqual(result.other_violations, ["something else"])

    def test_a_declared_integrity_assertion_is_a_discovery(self):
        case = {**CASE, "oracle": {**CASE["oracle"], "integrity": [{"assertion": "toy keeps writes", "evidence": "toy checked"}]}}
        search = self.campaign("i", bugs=[bug(80, ["toy holds"]), bug(40, ["toy keeps writes", "toy other"])], replays=["pass"])
        result = self.outcome(search, case)
        self.assertEqual(result.outcome, "discovery")
        self.assertEqual(result.assertion, "toy keeps writes")
        self.assertEqual(result.executions_to_first, 40)
        self.assertEqual(result.other_violations, ["toy holds", "toy other"])
        self.assertEqual(self.outcome(self.campaign("j", bugs=[bug(40, ["toy keeps writes"])])).outcome, "other-violation")

    def test_an_internal_assertion_at_the_fix_site_is_its_own_outcome(self):
        case = {**CASE, "oracle": {**CASE["oracle"], "fix_functions": ["walCheckpoint"]}}
        abort = bug(30, [discovery.NODE_EXIT])
        abort["replay"]["timeline"] = [
            {"console": "w: sqlite3.c:1: int walCheckpoint(Wal *): Assertion `x' failed.\nHS: 9 node 1 ended"}
        ]
        elsewhere = bug(30, [discovery.NODE_EXIT])
        elsewhere["replay"]["timeline"] = [{"console": "w: a.c:1: int other(void): Assertion `x' failed."}]
        self.assertEqual(self.outcome(self.campaign("a", bugs=[abort]), case).outcome, "internal-discovery")
        self.assertEqual(self.outcome(self.campaign("b", bugs=[elsewhere]), case).outcome, "other-violation")
        self.assertEqual(self.outcome(self.campaign("c", bugs=[abort])).outcome, "other-violation")

    def test_a_miss_needs_a_conclusive_check(self):
        self.assertEqual(self.outcome(self.campaign("a")).outcome, "miss")
        self.assertEqual(self.outcome(self.campaign("b", evidence=False)).outcome, "inconclusive")

    def test_infrastructure_failures_are_their_own_outcome(self):
        self.assertEqual(self.outcome(self.campaign("a", report=False)).outcome, "infra-failure")
        status = {"seed": 1, "execution_status": "infra_failure", "oracle": "fail: infra-failure (CLI exit 3)"}
        self.assertEqual(self.outcome(self.campaign("b", status=status)).outcome, "infra-failure")

    def test_the_table_censors_misses_at_the_budget(self):
        self.campaign("a", bugs=[bug(40, ["toy holds"])], replays=["pass"])
        self.campaign("b")
        self.campaign("c")
        self.campaign("d", case_id="toy-general")
        known = {"toy": CASE, "toy-general": GENERAL}
        table = discovery.render(discovery.collect(self.root, known), known)
        self.assertIn("| `toy` (focused) | 3 | 1 | > 60 | > 100 |", table)
        self.assertIn("| `toy-general` (general) | 1 | 0 |", table)


class Fisher(unittest.TestCase):
    def test_known_values(self):
        self.assertAlmostEqual(discovery.fisher_two_sided(10, 0, 0, 10), 1.0825e-05, places=8)
        self.assertAlmostEqual(discovery.fisher_two_sided(5, 5, 5, 5), 1.0)
        self.assertAlmostEqual(discovery.fisher_two_sided(3, 7, 0, 10), 0.2105, places=4)


if __name__ == "__main__":
    unittest.main()
