# SPDX-License-Identifier: AGPL-3.0-or-later
"""Segment panel scoring over synthetic matrix directories."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import segments  # noqa: E402

MANIFEST = {
    "id": "segments-test",
    "cases": [
        {"id": "named", "milestone": {"named": "bombs"}},
        {"id": "level", "milestone": {"at_least": {"progress.world": 1, "progress.level": 2}}},
        {"id": "unscored"},
    ],
}


def named_line(work, seen):
    return {
        "executions": work // 10,
        "execution_work": work,
        "workload_diagnostics": {"named_progress": {"first_seen": {"bombs": {"execution": 1} if seen else None}}},
    }


def level_line(work, world, level):
    return {"executions": work // 10, "execution_work": work, "progress": {"world": world, "level": level}}


def write_cell(matrix, case, seed, lines):
    campaign = matrix / f"{case}-s{seed}-w1-m512" / "campaign"
    campaign.mkdir(parents=True)
    (campaign / "progress.jsonl").write_text("".join(json.dumps(line) + "\n" for line in lines))


def write_named_arm(matrix, pass_works, censored_at, censored):
    for seed, work in enumerate(pass_works):
        write_cell(matrix, "named", seed, [named_line(work - 1, False), named_line(work, True)])
    for seed in range(len(pass_works), len(pass_works) + censored):
        write_cell(matrix, "named", seed, [named_line(censored_at, False)])


class SegmentsTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)

    def tearDown(self):
        self.directory.cleanup()

    def arm(self, name):
        matrix = self.root / name
        matrix.mkdir()
        return matrix

    def test_a_cell_is_timed_at_the_first_line_showing_its_milestone(self):
        matrix = self.arm("control")
        write_cell(matrix, "named", 1, [named_line(100, False), named_line(250, True), named_line(400, True)])
        write_cell(matrix, "named", 2, [named_line(100, False), named_line(900, False)])
        rows = segments.read_arm(matrix, {"named": {"named": "bombs"}})["named"]
        self.assertEqual(rows[0], {"seed": 1, "work": 250, "tries": 25, "passed": True})
        self.assertEqual(rows[1], {"seed": 2, "work": 900, "tries": 90, "passed": False})

    def test_a_list_of_names_is_reached_by_any_of_them(self):
        milestone = {"named": ["ice_beam", "bombs"]}
        self.assertTrue(segments.reached(milestone, named_line(1, True)))
        self.assertFalse(segments.reached(milestone, named_line(1, False)))
        self.assertEqual(segments.describe(milestone), "ice_beam or bombs")

    def test_progress_fields_compare_in_the_listed_order(self):
        milestone = MANIFEST["cases"][1]["milestone"]
        self.assertFalse(segments.reached(milestone, level_line(1, 1, 1)))
        self.assertTrue(segments.reached(milestone, level_line(1, 1, 2)))
        self.assertTrue(segments.reached(milestone, level_line(1, 2, 0)))
        self.assertFalse(segments.reached(milestone, level_line(1, 0, 3)))
        self.assertFalse(segments.reached(milestone, {"executions": 1, "execution_work": 1}))

    def test_the_median_is_unknown_when_most_seeds_are_censored(self):
        passed = [{"work": work, "passed": True} for work in (10, 20, 30)]
        censored = [{"work": 50, "passed": False} for _ in range(5)]
        self.assertIsNone(segments.median_work(passed + censored))
        self.assertEqual(segments.median_work(passed + censored[:1]), 20)

    def test_a_censored_seed_does_not_count_as_passing_at_its_budget(self):
        rows = [{"work": 10, "passed": True}, {"work": 10, "passed": False}, {"work": 30, "passed": True}]
        self.assertEqual(segments.median_work(rows), 30)

    def test_an_arm_that_passes_sooner_on_every_seed_is_faster(self):
        control, candidate = self.arm("control"), self.arm("candidate")
        write_named_arm(control, [900, 950], censored_at=1000, censored=14)
        write_named_arm(candidate, list(range(100, 1500, 100))[:14], censored_at=1500, censored=2)
        for matrix in (control, candidate):
            write_cell(matrix, "level", 1, [level_line(10, 1, 2)])
        report = segments.score(MANIFEST, [("control", control), ("candidate", candidate)])
        named = report["segments"][0]["arms"]
        self.assertEqual(named["control"]["passes"], 2)
        self.assertEqual(named["candidate"]["passes"], 14)
        self.assertEqual(named["candidate"]["verdict"], "faster")
        self.assertEqual(report["segments"][1]["arms"]["candidate"]["verdict"], "undecided")
        self.assertEqual(report["panel"]["candidate"]["verdict"], "faster")
        swapped = segments.score(MANIFEST, [("control", candidate), ("candidate", control)])
        self.assertEqual(swapped["segments"][0]["arms"]["candidate"]["verdict"], "slower")

    def test_identical_arms_are_undecided(self):
        control, candidate = self.arm("control"), self.arm("candidate")
        for matrix in (control, candidate):
            write_named_arm(matrix, [100, 200, 300, 400], censored_at=500, censored=4)
        report = segments.score(MANIFEST, [("control", control), ("candidate", candidate)])
        self.assertEqual(report["segments"][0]["arms"]["candidate"]["verdict"], "undecided")
        self.assertAlmostEqual(report["segments"][0]["arms"]["candidate"]["p"], 1.0)

    def test_one_arm_reports_whether_the_budget_is_calibrated(self):
        control = self.arm("control")
        write_named_arm(control, [100, 200, 300, 400], censored_at=500, censored=4)
        write_cell(control, "level", 1, [level_line(10, 0, 0)])
        text = segments.render(segments.score(MANIFEST, [("control", control)]))
        self.assertIn("4/8  passed", text)
        self.assertIn("calibrated", text)
        self.assertIn("budget too short", text)

    def test_a_malformed_milestone_is_refused(self):
        for milestone in ({"bogus": 1}, {"named": 3}, {"named": []}, {"named": ["a", 3]}, {"at_least": {}}, {"named": "a", "objective": True}, "bombs"):
            with self.assertRaises(ValueError):
                segments.describe(milestone)

    def test_a_manifest_without_milestones_is_refused(self):
        with self.assertRaises(ValueError):
            segments.score({"cases": [{"id": "unscored"}]}, [("control", self.arm("control"))])


if __name__ == "__main__":
    unittest.main()
