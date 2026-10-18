# SPDX-License-Identifier: AGPL-3.0-or-later
import unittest

from components import components


class SourceComponentTests(unittest.TestCase):
    def test_adjacent_fill_styles_share_a_region_but_playable_empty_space_does_not(self):
        metatiles = [{"name": "EMPTY", "collision": "COL_NONE"},
                     {"name": "X_NO_OUTLINE", "collision": "COL_NONE"},
                     {"name": "BLACK_NONE", "collision": "COL_NONE"},
                     {"name": "WALL", "collision": "COL_ALL"}]
        layout = {"width": 4, "height": 2, "tiles": [1, 2, 0, 3, 3, 3, 3, 1]}
        labels, groups = components(layout, metatiles)
        self.assertEqual(labels[0], labels[1])
        self.assertEqual(labels[2], -1)
        self.assertNotEqual(labels[0], labels[7])
        self.assertEqual(len(groups), 2)
        self.assertEqual(groups[0]["names"], {"X_NO_OUTLINE": 1, "BLACK_NONE": 1})
        self.assertEqual(groups[0]["open_faces"], [{"column": 1, "row": 0, "face": "right", "neighbor": 0}])
        self.assertEqual(groups[1]["open_faces"], [])


if __name__ == "__main__":
    unittest.main()
