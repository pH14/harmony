# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location("boundary_corners", Path(__file__).with_name("boundary-corners.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class BoundaryInventoryTests(unittest.TestCase):
    def test_quarter_block_beside_fill_is_a_review_lead_but_full_wall_is_not(self):
        index = {"metatiles": [{"name": "SMALL_BLOCK_TOP_RIGHT", "collision": "COL_UP_RIGHT"},
                               {"name": "BLACK_NONE", "collision": "COL_NONE"},
                               {"name": "BLOCK", "collision": "COL_ALL"}],
                 "levels": [{"side": "d", "level": 2, "name": "fixture", "source": "fixture.tmx",
                             "source_sha256": "fixture", "width": 2, "height": 2,
                             "tiles": [0, 2, 1, 2], "sprites": []}]}
        original = list(index["levels"][0]["tiles"])
        result = module.inventory(index)[0]
        self.assertEqual(len(result["quarter_caps"]), 1)
        lead = result["quarter_caps"][0]
        self.assertEqual((lead["column"], lead["row"], lead["unsealed_edge"]), (0, 1, "bottom"))
        self.assertEqual(lead["disposition"], "needs-input-and-intent-review")
        self.assertEqual(result["cap_components"][lead["component"]]["sprites"], [])
        self.assertEqual(index["levels"][0]["tiles"], original)


if __name__ == "__main__":
    unittest.main()
