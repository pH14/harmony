#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "counter_scan", Path(__file__).with_name("aa5-counter-scan.py")
)
scan = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scan)


class CounterScanTests(unittest.TestCase):
    def test_timer_value_reads_with_every_destination(self):
        for base, name in (
            (0xD53BE200, "CNTP_TVAL_EL0"),
            (0xD53BE300, "CNTV_TVAL_EL0"),
        ):
            for register in range(32):
                with self.subTest(name=name, register=register):
                    self.assertEqual(scan.decode_counter_read(base | register), name)

    def test_adjacent_registers_and_writes_are_not_counter_reads(self):
        for word in (0xD53BE220, 0xD53BE240, 0xD53BE320, 0xD53BE340,
                     0xD538E200, 0xD53AE200, 0xD51BE200, 0xD51BE300):
            with self.subTest(word=hex(word)):
                self.assertIsNone(scan.decode_counter_read(word))
        self.assertEqual(scan.decode_timer_program(0xD51BE200), "CNTP_TVAL_EL0")
        self.assertEqual(scan.decode_timer_program(0xD51BE300), "CNTV_TVAL_EL0")

    def check_scan(self, disassembly):
        words = [(0x1000, 0xD53BE201, ".text"), (0x1004, 0xD53BE31F, ".text")]
        output = subprocess.CompletedProcess([], 0, stdout=disassembly)
        with patch.object(scan, "executable_words", return_value=words), \
             patch.object(scan.subprocess, "run", return_value=output):
            reads, writes = scan.scan("fixture")
        self.assertEqual([read[3] for read in reads], ["CNTP_TVAL_EL0", "CNTV_TVAL_EL0"])
        self.assertEqual(writes, [])

    def test_real_mnemonics_agree_with_raw_decoder(self):
        self.check_scan("1000: d53be201 mrs x1, cntp_tval_el0\n"
                        "1004: d53be31f mrs xzr, cntv_tval_el0\n")

    def test_mapping_symbol_data_remains_rejected(self):
        self.check_scan("1000: d53be201 .word 0xd53be201\n"
                        "1004: d53be31f .inst 0xd53be31f\n")

    def test_word_omitted_from_disassembly_remains_rejected(self):
        self.check_scan("1000: d53be201 mrs x1, cntp_tval_el0\n")


if __name__ == "__main__":
    unittest.main()
