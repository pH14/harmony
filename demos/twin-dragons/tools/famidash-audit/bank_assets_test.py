# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("assets", Path(__file__).with_name("check-bank-assets.py"))
assets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assets)


class AssetTests(unittest.TestCase):
    def setUp(self):
        self.debug = 'seg\tid=1,name="PCM_BANK",start=0xA000,size=0x4,ooffs=16\n'
        self.rom = bytes(16) + b"abcd"

    def test_unchanged_asset_with_unrelated_appended_code(self):
        result = assets.compare(self.rom, self.debug, self.rom + b"code", self.debug, ["PCM_BANK"])
        self.assertEqual(result[0]["bytes"], 4)

    def test_equal_bytes_at_different_cpu_address_are_rejected(self):
        with self.assertRaisesRegex(AssertionError, "placement changed"):
            assets.compare(self.rom, self.debug, self.rom, self.debug.replace("0xA000", "0xA4C6"), ["PCM_BANK"])

    def test_equal_placement_with_different_bytes_is_rejected(self):
        with self.assertRaisesRegex(AssertionError, "bytes changed"):
            assets.compare(self.rom, self.debug, self.rom[:-1] + b"e", self.debug, ["PCM_BANK"])

    def test_missing_segment_is_rejected(self):
        with self.assertRaisesRegex(AssertionError, "missing asset segment"):
            assets.compare(self.rom, self.debug, self.rom, "", ["PCM_BANK"])


if __name__ == "__main__":
    unittest.main()
