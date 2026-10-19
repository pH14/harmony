# SPDX-License-Identifier: AGPL-3.0-or-later
import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("motion", Path(__file__).with_name("review-mini-wave45-motion.py"))
motion = importlib.util.module_from_spec(spec)
spec.loader.exec_module(motion)


class MotionTests(unittest.TestCase):
    def setUp(self):
        self.index = {"levels": [{"side": "main", "level": 20, "rom_sha256": "rom",
                                 "width": 2, "height": 2, "top": 0, "tiles": [0, 0, 1, 0]}],
                      "metatiles": [{"collision": "COL_NONE"}, {"collision": "COL_SLOPE_RD45"}]}
        state = {"state": 2, "dead": False, "debug": 0, "paused": 0, "invincible": 0}
        self.case = {"rom_sha256": "rom", "boot_level": 20, "options": {"side": "main"},
                     "trace": [dict(state), dict(state)]}
        self.cpu = {"schema": 2, "rom_sha256": "rom", "comparison_start_frame": 1,
                    "states": [dict(state, frame=f) for f in (1, 2)],
                    "samples": [self.sample(1, 0, 12, 10), self.sample(2, 0, 16, 17)]}

    @staticmethod
    def sample(frame, player, x, y):
        return {"frame": frame, "player": player, "sample_world_x": x, "sample_world_y": y,
                "centre_probe": True, "mode": 6, "mini": 1, "metatile": 0}

    def test_empty_endpoints_cross_intermediate_triangle(self):
        result = motion.review(self.index, self.case, self.cpu)
        self.assertEqual(len(result["intersections"]), 1)
        hit = result["intersections"][0]
        self.assertEqual(hit["parameter_interval"], ["9/11", "7/8"])
        self.assertEqual((hit["column"], hit["row"], hit["category"]), (0, 1, "live"))

    def test_dual_samples_do_not_replace_other_players_history(self):
        self.cpu["samples"].insert(1, self.sample(1, 1, 0, 0))
        self.cpu["samples"].append(self.sample(2, 1, 1, 0))
        hits = motion.review(self.index, self.case, self.cpu)["intersections"]
        self.assertEqual([h["player"] for h in hits], [0])

    def test_discontinuities_are_excluded(self):
        for changes in ({"frame": 3}, {"mode": 0}, {"mini": 0}, {"sample_world_x": 33}):
            cpu = copy.deepcopy(self.cpu)
            cpu["samples"][-1].update(changes)
            self.assertFalse(motion.review(self.index, self.case, cpu)["intersections"])

    def test_death_contact_is_separate_from_surviving_crossing(self):
        self.case["trace"][-1]["dead"] = self.cpu["states"][-1]["dead"] = True
        hit = motion.review(self.index, self.case, self.cpu)["intersections"][0]
        self.assertEqual(hit["category"], "death-frame")

    def test_source_disagreement_is_retained(self):
        self.cpu["samples"][-1]["metatile"] = 1
        result = motion.review(self.index, self.case, self.cpu)
        self.assertEqual(result["status"], "source-mismatch-requires-review")
        self.assertEqual(len(result["source_mismatches"]), 1)


if __name__ == "__main__":
    unittest.main()
