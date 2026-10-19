# SPDX-License-Identifier: AGPL-3.0-or-later
import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("centre_entries", Path(__file__).with_name("review-centre-entries.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class CentreEntryTest(unittest.TestCase):
    def fixture(self, dead=False):
        state = dict(state=2, dead=dead, debug=0, paused=0, invincible=0)
        level = dict(side="main", level=0, name="fixture", rom_sha256="rom", top=0,
                     width=3, height=3, tiles=[0, 0, 0, 0, 1, 0, 0, 0, 0])
        index = dict(levels=[level], metatiles=[dict(name="WALL", collision="COL_ALL"),
                                               dict(name="BLACK_NONE", collision="COL_NONE")])
        witness = dict(rom_sha256="rom", options=dict(side="main"), boot_level=0, trace=[state])
        sample = dict(frame=1, player=1, mode=6, mini=1, metatile=1,
                      sample_world_x=20, sample_world_y=20, centre_probe=True)
        cpu = dict(schema=2, rom_sha256="rom", comparison_start_frame=1,
                   states=[dict(frame=1, **state)], samples=[sample])
        return index, witness, cpu

    def test_death_frame_is_not_a_surviving_escape(self):
        result = module.review(*self.fixture(dead=True))
        group = result["components"]["0"]
        self.assertEqual(group["live_entries"], [])
        self.assertEqual(len(group["death_frame_entries"]), 1)
        self.assertFalse(result["source_review_complete"])

    def test_secondary_player_entry_is_retained(self):
        result = module.review(*self.fixture())
        self.assertEqual(result["components"]["0"]["live_entries"][0]["player"], 1)

    def test_stream_mismatch_is_not_assigned_source_intent(self):
        index, witness, cpu = self.fixture()
        cpu["samples"][0]["metatile"] = 0
        result = module.review(index, witness, cpu)
        self.assertEqual(result["status"], "source-mismatch-requires-review")
        self.assertEqual(result["components"], {})

    def test_reference_footprint_survives_replacement_with_solid_tiles(self):
        index, witness, cpu = self.fixture()
        reference = copy.deepcopy(index)
        index["levels"][0]["tiles"][4] = 0
        cpu["samples"][0]["metatile"] = 0
        result = module.review(index, witness, cpu, reference)
        self.assertEqual(result["region_geometry"], "reference index")
        self.assertEqual(result["components"]["0"]["live_entries"][0]["metatile"], 0)
        self.assertFalse(result["source_mismatches"])

    def test_reference_does_not_override_actual_source_validation(self):
        index, witness, cpu = self.fixture()
        reference = copy.deepcopy(index)
        index["levels"][0]["tiles"][4] = 0
        result = module.review(index, witness, cpu, reference)
        self.assertEqual(result["status"], "source-mismatch-requires-review")
        self.assertEqual(result["components"], {})

    def test_shifted_region_reference_is_rejected(self):
        index, witness, cpu = self.fixture()
        reference = copy.deepcopy(index)
        reference["levels"][0]["top"] = 16
        with self.assertRaisesRegex(AssertionError, "different level coordinates"):
            module.review(index, witness, cpu, reference)


if __name__ == "__main__":
    unittest.main()
