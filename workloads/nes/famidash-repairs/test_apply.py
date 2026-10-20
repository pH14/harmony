# SPDX-License-Identifier: AGPL-3.0-or-later
import base64
import importlib.util
from pathlib import Path
import tempfile
import unittest
import zlib

spec = importlib.util.spec_from_file_location("repairs", Path(__file__).with_name("apply.py"))
repairs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(repairs)


class PatchTest(unittest.TestCase):
    def fixture(self):
        original = bytes(repairs.ROM_BYTES)
        expected = bytearray(original)
        expected[40:43] = b"NES"
        entry = {"bytes": len(original), "input_sha256": repairs.digest(original),
                 "output_sha256": repairs.digest(expected), "blocks": [
                     {"offset": 40, "size": 3, "data": base64.b64encode(zlib.compress(b"NES")).decode()}]}
        return original, expected, entry

    def test_exact_delta(self):
        original, expected, entry = self.fixture()
        self.assertEqual(repairs.patch(original, entry), expected)

    def test_wrong_original(self):
        original, _, entry = self.fixture()
        with self.assertRaisesRegex(ValueError, "official ROM checksum"):
            repairs.patch(b"x" + original[1:], entry)

    def test_corrupt_payload(self):
        original, _, entry = self.fixture()
        entry["blocks"][0]["data"] = base64.b64encode(zlib.compress(b"BAD")).decode()
        with self.assertRaisesRegex(ValueError, "patched ROM checksum"):
            repairs.patch(original, entry)

    def test_overlapping_blocks(self):
        original, _, entry = self.fixture()
        entry["blocks"] *= 2
        with self.assertRaisesRegex(ValueError, "overlapping"):
            repairs.patch(original, entry)

    def test_decompression_exceeds_declared_size(self):
        original, _, entry = self.fixture()
        entry["blocks"][0]["data"] = base64.b64encode(zlib.compress(b"x" * 1000000)).decode()
        with self.assertRaisesRegex(ValueError, "compressed"):
            repairs.patch(original, entry)

    def test_all_inputs_validate_before_writing(self):
        original, _, entry = self.fixture()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            entries = []
            for name in sorted(repairs.FILES):
                (root / name).write_bytes(original)
                entries.append({**entry, "file": name})
            (root / entries[-1]["file"]).write_bytes(b"wrong")
            with self.assertRaisesRegex(ValueError, "ROM size"):
                repairs.apply({"schema": 1, "files": entries}, root, root / "output")
            self.assertFalse((root / "output").exists())

    def test_manifest_cannot_select_other_paths(self):
        with self.assertRaisesRegex(ValueError, "manifest"):
            repairs.apply({"schema": 1, "files": [{"file": "../elsewhere"}]}, Path("."), Path("."))

    def test_manifest_matches_download_and_runtime_pins(self):
        import json
        root = Path(__file__).resolve().parent
        manifest = json.loads((root / "roms.json").read_text())
        pins = dict(line.split("=", 1) for line in (root.parent / "famidash-versions.env").read_text().splitlines()
                    if line and not line.startswith("#"))
        for key, entry in zip(["FAMIDASH", "FAMIDASH_B_SIDES", "FAMIDASH_C_SIDES", "FAMIDASH_D_SIDES"], manifest["files"]):
            self.assertEqual(pins[key + "_UPSTREAM_ROM_SHA256"], entry["input_sha256"])
            self.assertEqual(pins[key + "_ROM_SHA256"], entry["output_sha256"])
        source = json.loads((root / "source-files.json").read_text())
        self.assertEqual(repairs.digest((root / "source.patch").read_bytes()), source["patch_sha256"])
