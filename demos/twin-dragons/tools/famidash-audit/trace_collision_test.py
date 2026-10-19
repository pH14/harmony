# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("trace_collision", Path(__file__).with_name("trace-collision.py"))
trace = importlib.util.module_from_spec(spec)
spec.loader.exec_module(trace)


class MappedCallerTests(unittest.TestCase):
    def test_same_cpu_caller_in_two_banks(self):
        base, caller = 0x10000000, 0xA450
        pages = [None] * 32
        pages[caller >> 11] = base + 7 * 8192 - 0xA000
        death_start = 16 + 7 * 8192 + 0x400
        first = trace.mapped_rom_offset(pages, base, 32 * 8192, caller)
        self.assertTrue(death_start <= first < death_start + 0x100)
        pages[caller >> 11] = base + 8 * 8192 - 0xA000
        other = trace.mapped_rom_offset(pages, base, 32 * 8192, caller)
        self.assertEqual(other - first, 8192)
        self.assertFalse(death_start <= other < death_start + 0x100)

    def test_retained_banked_helper_is_not_a_centre(self):
        path = Path(__file__).parent / "findings/shared-engine/banked-caller-classification-regression.json"
        case = json.loads(path.read_text())
        death = case["death_probe"]
        self.assertEqual(sum(sample["centre_probe"] for sample in case["samples"]), 1)
        for sample in case["samples"]:
            self.assertTrue(death["pc"] <= sample["return_address"] < death["pc"] + death["size"])
            actual = death["rom_offset"] <= sample["caller_rom_offset"] < death["rom_offset"] + death["size"]
            self.assertEqual(actual, sample["centre_probe"])

    def test_ram_and_unmapped_pages_have_no_rom_offset(self):
        pages = [None] * 32
        self.assertIsNone(trace.mapped_rom_offset(pages, 0x10000000, 32 * 8192, 0xA450))
        pages[0x6000 >> 11] = 0x20000000 - 0x6000
        self.assertIsNone(trace.mapped_rom_offset(pages, 0x10000000, 32 * 8192, 0x6000))
