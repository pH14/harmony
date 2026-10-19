# SPDX-License-Identifier: AGPL-3.0-or-later
import copy
import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location("control_regions", Path(__file__).with_name("review-control-regions.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ControlRegionTests(unittest.TestCase):
    def setUp(self):
        self.index = {"metatiles": [{"name": "BLACK_NONE", "collision": "COL_NONE"},
                                    {"name": "BLOCK", "collision": "COL_ALL"}],
                      "levels": [{"side": "d", "level": 2, "name": "fixture", "width": 3,
                                  "height": 1, "top": 0, "tiles": [0, 1, 0]}]}
        self.history = {"verified_frames": 1, "rows": [{"frame": 1,
                        "observation": {"x": 32, "dual": False},
                        "values": {"pit_previous_room": [0, 0], "pit_previous_x": [32, 0],
                                   "pit_previous_y": [0, 0], "pit_previous_metatile": [0, 0],
                                   "pit_previous_mode": [0, 0], "pit_previous_mini": [0, 0]}}]}

    def test_boundary_edits_do_not_hide_visits_to_the_original_region(self):
        reference = copy.deepcopy(self.index)
        reference["levels"][0]["tiles"] = [0, 0, 0]
        self.assertEqual(module.review(self.index, self.history, "d", 2, [(0, 0)], False, False)["status"], "avoids-regions")
        result = module.review(self.index, self.history, "d", 2, [(0, 0)], False, False, reference)
        self.assertEqual(result["status"], "visits-regions")
        self.assertEqual(result["source_history_mismatches"], [])

    def test_a_wrong_recorded_tile_prevents_a_clean_result(self):
        self.history["rows"][0]["values"]["pit_previous_metatile"][0] = 1
        result = module.review(self.index, self.history, "d", 2, [(0, 0)], False, False)
        self.assertEqual(result["status"], "source-history-mismatch")

    def test_secondary_player_visits_are_checked_independently(self):
        row = self.history["rows"][0]
        row["observation"] = {"x": 0, "dual": True}
        row["players"] = [{"player": 0, "x": 0}, {"player": 1, "x": 32}]
        row["values"]["pit_previous_x"] = [0, 32]
        result = module.review(self.index, self.history, "d", 2, [(2, 0)], False, False)
        self.assertEqual(result["status"], "visits-regions")
        self.assertEqual([hit["player"] for hit in result["hits"]], [1])

    def test_legacy_dual_history_cannot_report_a_clean_control(self):
        self.history["rows"][0]["observation"]["dual"] = True
        with self.assertRaisesRegex(AssertionError, "both world positions"):
            module.review(self.index, self.history, "d", 2, [(0, 0)], False, False)


if __name__ == "__main__":
    unittest.main()
