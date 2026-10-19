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

    def test_closed_region_can_lead_to_a_coin_outside_its_membership(self):
        metatiles = [{"name": "EMPTY", "collision": "COL_NONE"},
                     {"name": "BLACK_NONE", "collision": "COL_NONE"},
                     {"name": "WALL", "collision": "COL_ALL"}]
        tiles = [2] * 36
        tiles[13] = 1
        tiles[20] = tiles[23] = 0
        coin = {"column": 8, "row": 1, "id": 27, "name": "COIN3"}
        layout = {"width": 12, "height": 3, "tiles": tiles, "sprites": [coin,
                  {"column": 11, "row": 1, "id": 7, "name": "COIN1"},
                  {"column": 4, "row": 1, "id": 1, "name": "SHIP_MODE"}]}
        _, groups = components(layout, metatiles)
        self.assertEqual(groups[0]["sprites"], [])
        self.assertEqual(groups[0]["open_faces"], [])
        self.assertEqual(groups[0]["nearby_objectives"], [{**coin, "distance_tiles": 7}])


if __name__ == "__main__":
    unittest.main()
