#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Exercise missing, conflicting and failed case evidence in the nightly report."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("nightly_report", Path(__file__).with_name("nes-nightly-report.py"))
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)


class ReportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.suite = json.loads((REPORT.ROOT / "benchmarks/search/nightly.json").read_text())

    def write_roster(self, name, rows, suite=None):
        directory = self.root / name
        directory.mkdir()
        (directory / "suite.json").write_text(json.dumps(self.suite if suite is None else suite))
        (directory / "roster.json").write_text(json.dumps(rows))

    def complete_rows(self, media=None):
        if media is None:
            media = {"available": True, "mp4": "film/witness.mp4"}
        return [{"cell": job["id"], "case": job["case"]["id"], "status": "complete", "media": media}
                for job in REPORT.EVAL.expand_suite(self.suite)]

    def test_missing_jobs_remain_in_roster(self):
        rows, issues = REPORT.collect(self.suite, self.root)
        self.assertEqual(len(rows), 27)
        self.assertEqual(len(issues), 27)
        self.assertTrue(all(row["status"] == "missing" for row in rows))

    def test_failed_case_survives_merge(self):
        rows = self.complete_rows()
        rows[0]["status"] = "error"
        for index, row in enumerate(rows):
            self.write_roster(str(index), [row])
        merged, issues = REPORT.collect(self.suite, self.root)
        self.assertFalse(issues)
        self.assertEqual(merged[0]["status"], "error")
        self.assertIn("cases/0/index.html", REPORT.render(merged, issues))

    def test_media_a_case_never_rendered_is_an_issue_and_stays_visible(self):
        rows = self.complete_rows()
        rows[0]["media"] = {"available": False, "reason": "the search produced no renderable input"}
        for index, row in enumerate(rows):
            self.write_roster(str(index), [row])
        merged, issues = REPORT.collect(self.suite, self.root)
        self.assertEqual(len(issues), 1)
        self.assertIn("media unavailable: the search produced no renderable input", issues[0])
        page = REPORT.render(merged, issues)
        self.assertIn("no renderable input", page)
        self.assertIn("film with game audio", page)

    def test_a_roster_without_media_is_reported_rather_than_assumed(self):
        for index, row in enumerate(self.complete_rows(media=None)):
            del row["media"]
            self.write_roster(str(index), [row])
        _, issues = REPORT.collect(self.suite, self.root)
        self.assertEqual(len(issues), 27)
        self.assertTrue(all("predates media" in issue for issue in issues))

    def test_duplicate_cells_cannot_count_twice(self):
        rows = self.complete_rows()
        self.write_roster("one", rows)
        self.write_roster("two", rows[:1])
        merged, issues = REPORT.collect(self.suite, self.root)
        self.assertEqual(len(merged), 27)
        self.assertTrue(any("duplicate" in issue for issue in issues))

    def test_wrong_manifest_does_not_supply_evidence(self):
        self.write_roster("wrong", self.complete_rows(), {**self.suite, "id": "other"})
        rows, issues = REPORT.collect(self.suite, self.root)
        self.assertTrue(any("manifest differs" in issue for issue in issues))
        self.assertTrue(all(row["status"] == "missing" for row in rows))


if __name__ == "__main__":
    unittest.main()
