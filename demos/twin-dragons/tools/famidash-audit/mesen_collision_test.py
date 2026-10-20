# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("mesen_collision", Path(__file__).with_name("trace-mesen-collision.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class MesenCollisionTests(unittest.TestCase):
    def test_operands_are_not_interpreted_as_load_instructions(self):
        code = bytes.fromhex("a9 b1 a5 10 b1 bc 60")
        self.assertEqual([offset for offset, _ in module.instructions(code)], [0, 2, 4, 6])

    def test_indirect_pointer_wraps_in_zero_page(self):
        ram = bytearray(2048)
        ram[255], ram[0] = 0xF0, 0x60
        self.assertEqual(module.load_address(bytes.fromhex("b1 ff"), ram, 0, 32), 0x6110)

    def test_indexed_indirect_uses_the_x_register(self):
        ram = bytearray(2048)
        ram[0x10], ram[0x11] = 0x80, 0x62
        self.assertEqual(module.load_address(bytes.fromhex("a1 0e"), ram, 2, 99), 0x6280)

    def test_absolute_indexing_does_not_hide_page_crossings(self):
        self.assertEqual(module.load_address(bytes.fromhex("bd f0 63"), bytearray(2048), 32, 0), 0x6410)

    def test_unsupported_and_truncated_instructions_are_rejected(self):
        for code in (bytes.fromhex("02"), bytes.fromhex("b1")):
            with self.assertRaises(AssertionError):
                list(module.instructions(code))


if __name__ == "__main__":
    unittest.main()
