# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

try:
    from . import guest_files
except ImportError:  # unittest discovery can load this directory as top-level.
    import guest_files  # type: ignore[no-redef]


class GuestFileTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.root_fd = os.open(
            self.root,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
        )

    def tearDown(self) -> None:
        os.close(self.root_fd)
        self.temporary.cleanup()

    def assertRejected(self, relative: str, *, limit: int = guest_files.MAX_BYTES) -> None:
        with self.assertRaises(guest_files.GuestFileError):
            guest_files.read_json_at(self.root_fd, relative, limit)

    def test_normal_nested_json_and_root_descriptor_ownership(self) -> None:
        nested = self.root / "run"
        nested.mkdir()
        (nested / "report.json").write_text('{"ok": true, "n": 7}\n')

        self.assertEqual(
            guest_files.read_json_at(self.root_fd, "run/report.json"),
            {"ok": True, "n": 7},
        )
        os.fstat(self.root_fd)

    def test_intermediate_and_final_symlinks_are_rejected(self) -> None:
        real = self.root / "real"
        real.mkdir()
        (real / "report.json").write_text('{"ok": true}')
        (self.root / "link-dir").symlink_to(real, target_is_directory=True)
        (self.root / "link-file").symlink_to(real / "report.json")

        self.assertRejected("link-dir/report.json")
        self.assertRejected("link-file")

    def test_fifo_directory_and_other_nonregular_files_are_rejected(self) -> None:
        (self.root / "directory").mkdir()
        self.assertRejected("directory")
        if not hasattr(os, "mkfifo"):
            self.skipTest("FIFO creation is unavailable on this platform")
        os.mkfifo(self.root / "fifo")
        self.assertRejected("fifo")

    def test_cap_is_checked_against_actual_bytes(self) -> None:
        (self.root / "large.json").write_bytes(b"x" * 9)
        self.assertRejected("large.json", limit=8)
        (self.root / "exact.json").write_bytes(b"{}\n")
        self.assertEqual(guest_files.read_json_at(self.root_fd, "exact.json", 3), {})

    def test_duplicate_keys_nonobjects_and_nonfinite_values_are_rejected(self) -> None:
        (self.root / "duplicate.json").write_text('{"a": 1, "a": 2}')
        (self.root / "array.json").write_text("[]")
        (self.root / "nan.json").write_text('{"value": NaN}')
        self.assertRejected("duplicate.json")
        self.assertRejected("array.json")
        self.assertRejected("nan.json")

    def test_relative_path_must_be_normalized(self) -> None:
        (self.root / "report.json").write_text("{}")
        for relative in ("", "/report.json", "../report.json", "./report.json", "a/../report.json", "a//report.json", "report\\.json", "report\x00.json"):
            with self.subTest(relative=relative):
                self.assertRejected(relative)


if __name__ == "__main__":
    unittest.main()
