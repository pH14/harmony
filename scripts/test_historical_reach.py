#!/usr/bin/env python3
"""Tests for the historical reach diagnostic."""

from __future__ import annotations

import io
import json
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent


def add(archive: tarfile.TarFile, name: str, data: bytes) -> None:
    info = tarfile.TarInfo(name)
    info.size = len(data)
    archive.addfile(info, io.BytesIO(data))


class Reach(unittest.TestCase):
    def test_park_sites_resolve_through_the_image_symbols(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            layer = io.BytesIO()
            with tarfile.open(fileobj=layer, mode="w") as inner:
                add(inner, "symbols/server.sym.tsv", b"1000\tbuild_index\n2000\tprune_page\n")
                add(inner, "symbols/nested/ignored.sym.tsv", b"1000\twrong\n")
            with tarfile.open(root / "image.oci", "w") as outer:
                add(outer, "blobs/sha256/layer", layer.getvalue())
                add(outer, "index.json", b"{}")
            search = root / "case.search"
            search.mkdir()
            (search / "campaign-summary.json").write_text(
                json.dumps({"park_sites": {str(0x1004): 3, str(0x2010): 1, str(0x9000000): 2}})
            )
            case = root / "case.json"
            case.write_text(json.dumps({"oracle": {"fix_functions": ["build_index", "absent"]}}))
            output = subprocess.run(
                [sys.executable, "-I", str(HERE / "historical-reach.py"), "--symbols", str(root / "image.oci"),
                 "--case", str(case), str(search)],
                check=True, capture_output=True, text=True,
            ).stdout
        self.assertIn("park sites 3, resolved 2, functions 2", output)
        self.assertIn("build_index: reached (3 landings)", output)
        self.assertIn("absent: not reached (0 landings)", output)
        self.assertNotIn("wrong", output)


if __name__ == "__main__":
    unittest.main()
