# SPDX-License-Identifier: AGPL-3.0-or-later
"""Verify the bounded output mount used by the guest qualification canary.

The guest CLI runs on the host because it owns KVM, so its output directory
must be a controller-created tmpfs with finite byte and inode ceilings.  This
module only observes and validates that mount; it does not mount anything or
change the caller's process limits.
"""

from __future__ import annotations

import os
import stat
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


MAX_OUTPUT_BYTES = 512 * 1024 * 1024
MAX_OUTPUT_INODES = 16_384
MAX_MOUNTINFO_BYTES = 16 * 1024 * 1024
_READ_CHUNK_BYTES = 64 * 1024
_REQUIRED_OPTIONS = frozenset({"rw", "nodev", "nosuid", "noexec"})
_CONFLICTING_OPTIONS = (("rw", "ro"), ("dev", "nodev"), ("suid", "nosuid"), ("exec", "noexec"))


class GuestLimitError(ValueError):
    """The output directory is absent or does not have the required limits."""


@dataclass(frozen=True)
class MountInfo:
    """The fields of one Linux ``mountinfo`` row needed by this gate."""

    mount_id: int
    parent_id: int
    device: str
    root: str
    mount_point: str
    mount_options: tuple[str, ...]
    optional_fields: tuple[str, ...]
    filesystem: str
    source: str
    super_options: tuple[str, ...]

    @property
    def effective_options(self) -> frozenset[str]:
        """Return the comma-separated per-mount flags reported by the kernel."""

        return frozenset(self.mount_options)


def _fail(message: str) -> None:
    raise GuestLimitError(message)


def decode_mountinfo_path(value: str) -> str:
    """Decode Linux mountinfo's ``\\XYZ`` octal path escapes.

    Mountinfo escapes spaces, tabs, newlines, and backslashes as exactly three
    octal digits.  Rejecting every other backslash form prevents a malformed
    synthetic row from being silently matched to the requested directory.
    """

    if type(value) is not str:
        _fail("mountinfo paths must be text")
    output: list[str] = []
    index = 0
    while index < len(value):
        character = value[index]
        if character != "\\":
            output.append(character)
            index += 1
            continue
        escaped = value[index + 1 : index + 4]
        if len(escaped) != 3 or any(character not in "01234567" for character in escaped):
            _fail("mountinfo path contains a malformed octal escape")
        output.append(chr(int(escaped, 8)))
        index += 4
    decoded = "".join(output)
    if "\x00" in decoded:
        _fail("mountinfo path contains NUL")
    return decoded


def _positive_decimal(value: str, name: str, *, allow_zero: bool = False) -> int:
    if type(value) is not str or not value.isascii() or not value.isdecimal():
        _fail(f"mountinfo {name} is not a decimal integer")
    number = int(value)
    if number == 0 and allow_zero:
        return number
    if number <= 0:
        _fail(f"mountinfo {name} must be positive")
    return number


def _device(value: str) -> str:
    major, separator, minor = value.partition(":")
    if not separator or ":" in minor:
        _fail("mountinfo device is malformed")
    _positive_decimal(major, "major", allow_zero=True)
    _positive_decimal(minor, "minor", allow_zero=True)
    return value


def _options(value: str, name: str) -> tuple[str, ...]:
    values = tuple(value.split(","))
    if not value or any(not item for item in values) or len(set(values)) != len(values):
        _fail(f"mountinfo {name} contains empty or duplicate options")
    return values


def parse_mountinfo(text: str) -> tuple[MountInfo, ...]:
    """Parse Linux mountinfo rows without consulting the host filesystem."""

    if type(text) is not str:
        _fail("mountinfo must be text")
    records: list[MountInfo] = []
    # Mountinfo uses ASCII LF and spaces as delimiters.  ``splitlines`` and
    # ``str.split`` also treat some decoded UTF-8 characters as delimiters,
    # which would corrupt a legitimate non-ASCII mountpoint.
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    for line_number, line in enumerate(lines, 1):
        if not line:
            _fail(f"mountinfo row {line_number} is empty")
        fields = [field for field in line.split(" ") if field]
        if len(fields) < 10:
            _fail(f"mountinfo row {line_number} is too short")
        try:
            separator = fields.index("-", 6)
        except ValueError:
            _fail(f"mountinfo row {line_number} has an invalid separator")
        if len(fields) != separator + 4:
            _fail(f"mountinfo row {line_number} has malformed filesystem fields")
        root = decode_mountinfo_path(fields[3])
        mount_point = decode_mountinfo_path(fields[4])
        if not root.startswith("/") or not mount_point.startswith("/"):
            _fail(f"mountinfo row {line_number} has a relative path")
        filesystem, source, super_options = fields[separator + 1 : separator + 4]
        if not filesystem or not source:
            _fail(f"mountinfo row {line_number} has an empty filesystem field")
        records.append(
            MountInfo(
                mount_id=_positive_decimal(fields[0], "mount id"),
                parent_id=_positive_decimal(fields[1], "parent id"),
                device=_device(fields[2]),
                root=root,
                mount_point=mount_point,
                mount_options=_options(fields[5], "mount options"),
                optional_fields=tuple(fields[6:separator]),
                filesystem=filesystem,
                source=source,
                super_options=_options(super_options, "super options"),
            )
        )
    return tuple(records)


def _absolute_path(value: os.PathLike[str] | str) -> str:
    try:
        raw = os.fspath(value)
    except TypeError as exc:
        raise GuestLimitError("mount path must be a text path") from exc
    if isinstance(raw, bytes) or "\x00" in raw or not os.path.isabs(raw):
        _fail("mount path must be an absolute NUL-free text path")
    return os.path.normpath(raw)


def _capacity(total_bytes: object, total_inodes: object) -> tuple[int, int]:
    if type(total_bytes) is not int or total_bytes <= 0 or total_bytes > MAX_OUTPUT_BYTES:
        _fail("output mount byte capacity is outside the positive 512 MiB bound")
    if type(total_inodes) is not int or total_inodes <= 0 or total_inodes > MAX_OUTPUT_INODES:
        _fail("output mount inode capacity is outside the positive 16384 bound")
    return total_bytes, total_inodes


def _provenance(record: MountInfo, total_bytes: int, total_inodes: int) -> dict[str, object]:
    return {
        "mount_path": record.mount_point,
        "fs": record.filesystem,
        "bytes": total_bytes,
        "inodes": total_inodes,
        "options": sorted(record.effective_options),
    }


def validate_mount(
    records: Iterable[MountInfo],
    parent: os.PathLike[str] | str,
    *,
    total_bytes: int,
    total_inodes: int,
) -> dict[str, object]:
    """Validate a parsed exact mountpoint and synthetic capacity values.

    This pure helper is intentionally separate from :func:`verify_output_mount`
    so portable tests can plant filesystem, flag, path, and capacity failures
    without pretending to have observed a Linux mount.
    """

    expected = _absolute_path(parent)
    try:
        rows = tuple(records)
    except TypeError as exc:
        raise GuestLimitError("mount records must be iterable") from exc
    matches = [record for record in rows if isinstance(record, MountInfo) and record.mount_point == expected]
    if len(matches) != 1:
        _fail("output directory is absent from mountinfo or has duplicate mount rows")
    record = matches[0]
    if record.filesystem != "tmpfs":
        _fail("output directory is not mounted as tmpfs")
    options = record.effective_options
    if not _REQUIRED_OPTIONS <= options:
        _fail("output tmpfs is missing a required effective mount flag")
    if any(first in options and second in options for first, second in _CONFLICTING_OPTIONS):
        _fail("output tmpfs has contradictory effective mount flags")
    checked_bytes, checked_inodes = _capacity(total_bytes, total_inodes)
    return _provenance(record, checked_bytes, checked_inodes)


def validate_mountinfo(
    text: str,
    parent: os.PathLike[str] | str,
    *,
    total_bytes: int,
    total_inodes: int,
) -> dict[str, object]:
    """Parse and validate mountinfo text with supplied statvfs values."""

    return validate_mount(parse_mountinfo(text), parent, total_bytes=total_bytes, total_inodes=total_inodes)


def _read_mountinfo() -> str:
    chunks: list[bytes] = []
    total = 0
    try:
        with Path("/proc/self/mountinfo").open("rb") as stream:
            while True:
                chunk = stream.read(min(_READ_CHUNK_BYTES, MAX_MOUNTINFO_BYTES + 1 - total))
                if not chunk:
                    break
                total += len(chunk)
                if total > MAX_MOUNTINFO_BYTES:
                    _fail("mountinfo exceeds its input bound")
                chunks.append(chunk)
    except GuestLimitError:
        raise
    except OSError as exc:
        raise GuestLimitError("cannot read /proc/self/mountinfo") from exc
    # Linux escapes only ASCII space, tab, newline, and backslash in paths;
    # other UTF-8 bytes remain literal.  fsdecode preserves undecodable bytes
    # with surrogateescape so exact mountpoint comparison still works.
    return os.fsdecode(b"".join(chunks))


def verify_output_mount(parent: Path) -> dict[str, object]:
    """Verify the existing exact tmpfs mount containing a guest output path."""

    if sys.platform != "linux":
        _fail("bounded guest output mounts are supported only on Linux")
    try:
        resolved = Path(parent).resolve(strict=True)
    except (OSError, RuntimeError, TypeError) as exc:
        raise GuestLimitError("output mount parent cannot be resolved") from exc
    try:
        information = os.stat(resolved, follow_symlinks=False)
    except OSError as exc:
        raise GuestLimitError("output mount parent cannot be inspected") from exc
    if not stat.S_ISDIR(information.st_mode):
        _fail("output mount parent is not a directory")

    records = parse_mountinfo(_read_mountinfo())
    matches = [record for record in records if record.mount_point == str(resolved)]
    if len(matches) != 1:
        _fail("output mount parent is not one exact mountpoint")
    record = matches[0]
    try:
        usage = os.statvfs(resolved)
    except OSError as exc:
        raise GuestLimitError("output mount capacity cannot be inspected") from exc
    if type(usage.f_blocks) is not int or type(usage.f_frsize) is not int:
        _fail("output mount reports invalid byte capacity fields")
    total_bytes = usage.f_blocks * usage.f_frsize
    total_inodes = usage.f_files
    return validate_mount(
        (record,),
        resolved,
        total_bytes=total_bytes,
        total_inodes=total_inodes,
    )


__all__ = [
    "GuestLimitError",
    "MAX_OUTPUT_BYTES",
    "MAX_OUTPUT_INODES",
    "MountInfo",
    "decode_mountinfo_path",
    "parse_mountinfo",
    "validate_mount",
    "validate_mountinfo",
    "verify_output_mount",
]
