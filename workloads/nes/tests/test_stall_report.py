# SPDX-License-Identifier: AGPL-3.0-or-later
import copy
import argparse
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("stall_report", Path(__file__).parents[1] / "stall-report.py")
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)


def snapshot(value=0, identity="a"):
    work = [0] * 2048
    work[0x45] = value
    return {"state_sha256": identity, "decoded_ram": {"work": work, "save": []},
            "observation": {"decoded": {"y": value}}}


def entry(cell="cell", state=None, selected=10, productive=0):
    return {"id": 1, "tier": "tier", "cell": cell, "active": True,
            "selector": {"selected": selected, "productive": productive}, "snapshot": state}


def record(execution, draws, yields=1, census=True):
    selector = {"tier_runs": {"tier": {"draws": draws, "yields": yields, "dry": 2}}}
    if census:
        selector["draws_by_cell"] = {"cell": draws}
    return {"executions": execution, "selector": selector}


def metadata(final):
    return {"header": {"executions": final["executions"]}, "checkpoint_sha256": "fixture",
            "historical_entries_dropped": 0, "draws_by_cell": final["selector"]["draws_by_cell"],
            "tier_runs": final["selector"]["tier_runs"]}


class StallReportTests(unittest.TestCase):
    def test_named_constant_and_duplicate_state_counts(self):
        state = snapshot(16)
        result = REPORT.variation([entry(state=state), entry(state=copy.deepcopy(state))], [("$0045", "health")])
        self.assertEqual(result["distinct_snapshot_states"], 1)
        self.assertEqual(result["retained_entries"], 2)
        self.assertEqual(result["requested_bytes"][0]["status"], "constant")
        self.assertEqual(result["requested_bytes"][0]["name"], "health")
        self.assertEqual(result["requested_bytes"][0]["values"], [{"value": 16, "count": 1}])
        self.assertEqual(result["top_ten_ram"], [])

    def test_missing_states_do_not_certify_a_cell_constant(self):
        result = REPORT.variation([entry(state=snapshot(16)), entry()], [("$0045", "health")])
        self.assertEqual(result["requested_bytes"][0]["status"], "constant_in_available_states")
        self.assertEqual(result["missing_snapshots"], 1)
        unavailable = REPORT.variation([entry()], [("$0045", None)])
        self.assertEqual(unavailable["requested_bytes"][0]["status"], "unavailable")

    def test_variation_ranks_observations_and_ram_independently(self):
        result = REPORT.variation([entry(state=snapshot(1, "a")), entry(state=snapshot(2, "b"))], [])
        self.assertEqual(result["top_ten_ram"][0]["field"], "$0045")
        self.assertEqual(result["top_ten_observations"][0]["field"], "decoded.y")
        self.assertEqual(result["top_ten_ram"][0]["entropy_bits"], 1)

    def test_attribution_requires_two_censuses_inside_final_yield_streak(self):
        rows = [record(1, 1, 0), record(2, 2), record(3, 4, census=False), record(4, 10)]
        result = REPORT.report(metadata(rows[-1]), [entry()], iter(rows), [])
        tier = result["tiers"][0]
        self.assertEqual(tier["cell_draw_window_start"], 2)
        self.assertEqual(tier["cells"][0]["draws"], 8)
        self.assertEqual(tier["cells"][0]["share_of_tier_draws"], 1)
        self.assertEqual(tier["verified_zero_retention_cells"], ["cell"])

    def test_sparse_census_does_not_invent_draw_shares(self):
        rows = [record(1, 1, census=False), record(4, 10)]
        tier = REPORT.report(metadata(rows[-1]), [entry()], rows, [])["tiers"][0]
        self.assertIsNone(tier["cells"][0]["draws"])
        self.assertIsNone(tier["cells"][0]["share_of_tier_draws"])
        self.assertEqual(tier["cells"][0]["lifetime_draws"], 10)

    def test_tier_yield_cannot_certify_source_cell_productivity(self):
        rows = [record(1, 1), record(4, 10)]
        tier = REPORT.report(metadata(rows[-1]), [entry(productive=2)], rows, [])["tiers"][0]
        self.assertEqual(tier["verified_zero_retention_cells"], [])
        self.assertIsNone(tier["cells"][0]["interval_retention_yield"])
        tier = REPORT.report(metadata(rows[-1]), [entry(selected=3)], rows, [])["tiers"][0]
        self.assertFalse(tier["cells"][0]["entry_draw_accounting_complete"])
        self.assertFalse(tier["cells"][0]["verified_zero_retention_draws"])

    def test_unmapped_cells_remain_in_tier_denominator(self):
        rows = [record(1, 2), record(4, 10)]
        rows[0]["selector"]["draws_by_cell"] = {"cell": 1, "lost": 1}
        rows[1]["selector"]["draws_by_cell"] = {"cell": 5, "lost": 5}
        tier = REPORT.report(metadata(rows[-1]), [entry(selected=5)], rows, [])["tiers"][0]
        self.assertEqual(tier["cells"][0]["share_of_tier_draws"], 0.5)
        self.assertEqual(tier["cells"][0]["share_of_attributed_tier_draws"], 1)

    def test_mismatched_inputs_are_rejected(self):
        rows = [record(1, 1), record(4, 10)]
        for field in ["header", "draws_by_cell", "tier_runs"]:
            wrong = copy.deepcopy(metadata(rows[-1]))
            wrong[field] = {"executions": 9} if field == "header" else {}
            with self.assertRaises(ValueError):
                REPORT.report(wrong, [entry()], rows, [])

    def test_progress_sequence_and_ram_lengths_are_checked(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "progress.jsonl"
            path.write_text('{"executions": 2}\n{"executions": 1}\n')
            with self.assertRaises(ValueError):
                list(REPORT.read_progress(path))
        bad = snapshot()
        bad["decoded_ram"]["work"].pop()
        with self.assertRaises(ValueError):
            REPORT.ram(bad)
        with self.assertRaises(argparse.ArgumentTypeError):
            REPORT.diagnostic("8000")


if __name__ == "__main__":
    unittest.main()
