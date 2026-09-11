# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import json
import sys
import unittest
from dataclasses import replace
from pathlib import Path
from unittest import mock

try:
    from . import guest_limits
except ImportError:  # unittest discovery can load this directory as top-level.
    import guest_limits  # type: ignore[no-redef]


BOUND = "/reports/skill-guest/bounded"


def mountinfo(*, filesystem: str = "tmpfs", options: str = "rw,nosuid,nodev,noexec,relatime") -> str:
    return "\n".join(
        (
            "25 20 8:1 / / ro,relatime - ext4 /dev/root rw,errors=remount-ro",
            f"26 20 0:42 / {BOUND} {options} shared:5 - {filesystem} tmpfs rw,size=512m,nr_inodes=16384,mode=700",
        )
    )


class GuestLimitTests(unittest.TestCase):
    def parsed(self, text: str | None = None) -> guest_limits.MountInfo:
        rows = guest_limits.parse_mountinfo(text if text is not None else mountinfo())
        return next(row for row in rows if row.mount_point == BOUND)

    def assertRejected(self, function, *args, **kwargs) -> None:  # type: ignore[no-untyped-def]
        with self.assertRaises(guest_limits.GuestLimitError):
            function(*args, **kwargs)

    def test_octal_mountinfo_paths_decode_without_prefix_matching(self) -> None:
        self.assertEqual(
            guest_limits.decode_mountinfo_path(r"/tmp/a\040b\011c\012d\134e"),
            "/tmp/a b\tc\nd\\e",
        )
        for malformed in (r"/tmp/a\04", r"/tmp/a\09x", "/tmp/a\\134x\\"):
            with self.subTest(malformed=malformed):
                self.assertRejected(guest_limits.decode_mountinfo_path, malformed)

    def test_parser_handles_optional_fields_and_rejects_malformed_rows(self) -> None:
        row = self.parsed()
        self.assertEqual(row.device, "0:42")
        self.assertEqual(row.optional_fields, ("shared:5",))
        self.assertEqual(row.root, "/")
        self.assertEqual(row.mount_options[:4], ("rw", "nosuid", "nodev", "noexec"))
        unicode_mount = mountinfo().replace(BOUND, "/tmp/café\u00a0x\u2028y", 1)
        self.assertEqual(guest_limits.parse_mountinfo(unicode_mount)[1].mount_point, "/tmp/café\u00a0x\u2028y")
        malformed = (
            "25 20 8:1 / / ro,relatime ext4 /dev/root rw",
            mountinfo().replace("shared:5 - tmpfs tmpfs", "shared:5 tmpfs tmpfs", 1),
            mountinfo().replace("shared:5 - tmpfs tmpfs", "shared:5 - tmpfs", 1),
        )
        for text in malformed:
            with self.subTest(text=text):
                self.assertRejected(guest_limits.parse_mountinfo, text)
        # Empty input has no rows; the validator rejects it as an absent mount.
        self.assertEqual(guest_limits.parse_mountinfo(""), ())
        self.assertEqual(guest_limits.parse_mountinfo(mountinfo() + "\n")[1].mount_point, BOUND)

    def test_validate_returns_json_safe_mount_provenance(self) -> None:
        provenance = guest_limits.validate_mountinfo(
            mountinfo(), BOUND, total_bytes=guest_limits.MAX_OUTPUT_BYTES, total_inodes=guest_limits.MAX_OUTPUT_INODES
        )
        self.assertEqual(
            provenance,
            {
                "mount_path": BOUND,
                "fs": "tmpfs",
                "bytes": guest_limits.MAX_OUTPUT_BYTES,
                "inodes": guest_limits.MAX_OUTPUT_INODES,
                "options": ["nodev", "noexec", "nosuid", "relatime", "rw"],
            },
        )
        json.dumps(provenance)

    def test_exact_path_type_flags_and_duplicate_rows_are_required(self) -> None:
        record = self.parsed()
        for path in ("/reports/skill-guest", BOUND + "/child"):
            with self.subTest(path=path):
                self.assertRejected(
                    guest_limits.validate_mount,
                    (record,),
                    path,
                    total_bytes=1,
                    total_inodes=1,
                )
        self.assertRejected(
            guest_limits.validate_mount,
            (replace(record, filesystem="overlay"),),
            BOUND,
            total_bytes=1,
            total_inodes=1,
        )
        self.assertRejected(
            guest_limits.validate_mount,
            (record, record),
            BOUND,
            total_bytes=1,
            total_inodes=1,
        )

    def test_required_flags_and_contradictions_are_rejected(self) -> None:
        record = self.parsed()
        for option in ("rw", "nosuid", "nodev", "noexec"):
            options = tuple(item for item in record.mount_options if item != option)
            with self.subTest(option=option):
                self.assertRejected(
                    guest_limits.validate_mount,
                    (replace(record, mount_options=options),),
                    BOUND,
                    total_bytes=1,
                    total_inodes=1,
                )
        for option in (("rw", "ro"), ("nodev", "dev"), ("nosuid", "suid"), ("noexec", "exec")):
            with self.subTest(option=option):
                options = record.mount_options + (option[1],)
                if option[0] == "rw":
                    options = record.mount_options + ("ro",)
                self.assertRejected(
                    guest_limits.validate_mount,
                    (replace(record, mount_options=options),),
                    BOUND,
                    total_bytes=1,
                    total_inodes=1,
                )

    def test_capacity_is_positive_and_bounded(self) -> None:
        record = self.parsed()
        capacities = (
            (0, 1),
            (guest_limits.MAX_OUTPUT_BYTES + 1, 1),
            (1, 0),
            (1, guest_limits.MAX_OUTPUT_INODES + 1),
            (True, 1),
            (1, False),
        )
        for total_bytes, total_inodes in capacities:
            with self.subTest(total_bytes=total_bytes, total_inodes=total_inodes):
                self.assertRejected(
                    guest_limits.validate_mount,
                    (record,),
                    BOUND,
                    total_bytes=total_bytes,
                    total_inodes=total_inodes,
                )

    def test_non_linux_refuses_before_filesystem_access(self) -> None:
        with mock.patch.object(guest_limits.sys, "platform", "darwin"):
            self.assertRejected(guest_limits.verify_output_mount, Path("/does/not/exist"))


if __name__ == "__main__":
    unittest.main()
