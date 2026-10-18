# SPDX-License-Identifier: AGPL-3.0-or-later
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("verify_geometry", Path(__file__).with_name("verify-geometry.py"))
geometry = importlib.util.module_from_spec(spec)
spec.loader.exec_module(geometry)


class RomGeometryTests(unittest.TestCase):
    def test_lz_copy_uses_snapshot_before_writing_dictionary(self):
        rom = bytes(16) + bytes([128, 65, 3, 0, 66, 255])
        self.assertEqual(list(geometry.decompress(rom, 16)), [65, 65, 0, 0, 66])

    def test_continuation_is_read_from_actual_rom_table(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "main").mkdir()
            rom = bytearray(16 + 8192)
            rom[16:22] = bytes([0, 0xa1, 0, 0, 0xa2, 0])
            rom[272 + 17] = 1
            first = bytes([1, 1, 0x82, 127, 127, 0])
            second = bytes([2, 3])
            for offset, data in [(290, first), (528, second)]:
                packed = bytes(value for byte in data for value in (128, byte)) + bytes([255])
                rom[offset:offset + len(packed)] = packed
            path = root / "test.nes"
            path.write_bytes(rom)
            names = ["_level_list_lo", "_level_list_hi", "_level_list_bank",
                     "_level_chunk_list_lo", "_level_chunk_list_hi", "_level_chunk_list_bank"]
            debug = 'seg\tid=0,name="DATA",start=0xA000,ooffs=16\n'
            debug += "".join(f'sym\tname="{name}",seg=0,val=0x{0xa000 + i:X}\n'
                             for i, name in enumerate(names))
            (root / "main" / "famidash.dbg").write_text(debug)
            layout = {"side": "main", "level": 0, "name": "test", "rom": path.name,
                      "rom_sha256": hashlib.sha256(rom).hexdigest(), "width": 6,
                      "height": 1, "tiles": [1, 1, 2, 3, 3, 3]}
            result = geometry.compare({"levels": [layout]}, root, root)
            self.assertEqual(result["status"], "verified")
            self.assertEqual(result["levels"][0]["continuations"], [0])
            layout["tiles"][-1] = 4
            result = geometry.compare({"levels": [layout]}, root, root)
            self.assertEqual(result["status"], "geometry-mismatch")
            self.assertEqual(result["levels"][0]["examples"],
                             [{"column": 5, "row": 0, "source": 4, "rom": 3}])


if __name__ == "__main__":
    unittest.main()
