# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location("wave66_dispatch", Path(__file__).with_name("check-wave66-dispatch.py"))
dispatch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dispatch)


class DirectCallBankTest(unittest.TestCase):
    def fixture(self, callee_address=0xA100, callee_offset=272):
        rom = bytearray(20000)
        rom[16:20] = bytes([0x20, callee_address & 255, callee_address >> 8, 0x60])
        symbols = {"caller": {"val": "0xA000", "seg": "1", "size": "4"},
                   "callee": {"val": hex(callee_address), "seg": "2", "size": "1"}}
        segments = {1: {"start": "0xA000", "ooffs": "16"},
                    2: {"start": hex(callee_address), "ooffs": str(callee_offset)}}
        return rom, symbols, segments

    def test_distinct_segments_in_same_physical_bank(self):
        result = dispatch.check_direct_call_bank(*self.fixture(), "caller", "callee")
        self.assertEqual(result["caller_prg_bank"], result["callee_prg_bank"])

    def test_same_cpu_window_in_different_physical_bank(self):
        with self.assertRaisesRegex(AssertionError, "bank mismatch"):
            dispatch.check_direct_call_bank(*self.fixture(callee_offset=8464), "caller", "callee")

    def test_fixed_callee_window(self):
        dispatch.check_direct_call_bank(*self.fixture(callee_address=0xF100, callee_offset=8464), "caller", "callee")

    def test_requires_actual_call_reference(self):
        rom, symbols, segments = self.fixture()
        rom[16] = 0xEA
        with self.assertRaisesRegex(AssertionError, "no direct call"):
            dispatch.check_direct_call_bank(rom, symbols, segments, "caller", "callee")
