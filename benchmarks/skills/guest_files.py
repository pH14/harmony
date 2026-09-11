# SPDX-License-Identifier: AGPL-3.0-or-later
"""Read controller-owned guest evidence through an anchored directory fd.

The caller opens the evidence root before starting the guest process and keeps
that descriptor alive.  This module only opens descendants relative to it;
the root descriptor remains owned by the caller.
"""

from __future__ import annotations

import json
import os
import stat
from typing import Any

try:
    from . import build
except ImportError:  # unittest discovery can load this directory as top-level.
    import build  # type: ignore[no-redef]


MAX_BYTES = 16 * 1024 * 1024
_CHUNK_BYTES = 64 * 1024


class GuestFileError(ValueError):
    """The requested guest evidence path or document is unsafe or invalid."""


def _flags(*names: str) -> int:
    try:
        flags = os.O_RDONLY
        for name in names:
            flags |= getattr(os, name)
        return flags
    except AttributeError as exc:
        raise GuestFileError("the platform lacks required no-follow file flags") from exc


def _relative_parts(relative: str) -> list[str]:
    try:
        normalized = build._output_path(relative)
    except (TypeError, UnicodeError, ValueError) as exc:
        raise GuestFileError("relative evidence path is not normalized") from exc
    return normalized.split("/")


def _read_bytes_at(root_fd: int, parts: list[str], limit: int) -> bytes:
    directory_flags = _flags("O_DIRECTORY", "O_NOFOLLOW")
    file_flags = _flags("O_NONBLOCK", "O_NOFOLLOW")
    opened_directories: list[int] = []
    file_fd: int | None = None
    current_fd = root_fd
    try:
        try:
            root_info = os.fstat(root_fd)
        except OSError as exc:
            raise GuestFileError("evidence root descriptor cannot be inspected") from exc
        if not stat.S_ISDIR(root_info.st_mode):
            raise GuestFileError("evidence root descriptor is not a directory")

        for component in parts[:-1]:
            try:
                descriptor = os.open(component, directory_flags, dir_fd=current_fd)
            except OSError as exc:
                raise GuestFileError("evidence path contains an unsafe directory") from exc
            opened_directories.append(descriptor)
            current_fd = descriptor

        try:
            file_fd = os.open(parts[-1], file_flags, dir_fd=current_fd)
        except OSError as exc:
            raise GuestFileError("evidence file cannot be opened without following links") from exc
        try:
            info = os.fstat(file_fd)
        except OSError as exc:
            raise GuestFileError("evidence file cannot be inspected") from exc
        if not stat.S_ISREG(info.st_mode):
            raise GuestFileError("evidence file is not a regular file")
        if info.st_size > limit:
            raise GuestFileError("evidence file exceeds its byte limit")

        data = bytearray()
        while len(data) <= limit:
            try:
                chunk = os.read(file_fd, min(_CHUNK_BYTES, limit + 1 - len(data)))
            except OSError as exc:
                raise GuestFileError("evidence file cannot be read") from exc
            if not chunk:
                break
            data.extend(chunk)
        if len(data) > limit:
            raise GuestFileError("evidence file exceeds its byte limit")
        return bytes(data)
    finally:
        if file_fd is not None:
            os.close(file_fd)
        for descriptor in reversed(opened_directories):
            os.close(descriptor)


def _reject_constant(value: str) -> None:
    raise ValueError(f"non-finite JSON number {value}")


def read_json_at(
    root_fd: int,
    relative: str,
    limit: int = MAX_BYTES,
) -> dict[str, Any]:
    """Read one normalized relative JSON object below ``root_fd``.

    ``root_fd`` remains open and owned by the caller.  Every descendant
    descriptor opened here is closed before returning or raising.  The final
    file is opened with ``O_NONBLOCK|O_NOFOLLOW``, checked with ``fstat`` for
    regular-file identity, and read through ``limit + 1`` bytes so a file that
    grows after the check cannot evade the cap.
    """

    if type(root_fd) is not int or root_fd < 0:
        raise GuestFileError("root_fd must be a nonnegative file descriptor")
    if type(limit) is not int or not 0 <= limit <= MAX_BYTES:
        raise GuestFileError("limit must be an integer between zero and 16 MiB")
    parts = _relative_parts(relative)
    data = _read_bytes_at(root_fd, parts, limit)
    try:
        document = json.loads(
            data.decode("utf-8"),
            object_pairs_hook=build._unique_object,
            parse_constant=_reject_constant,
        )
    except (UnicodeDecodeError, ValueError, json.JSONDecodeError) as exc:
        raise GuestFileError("evidence file is not valid JSON") from exc
    if type(document) is not dict:
        raise GuestFileError("evidence JSON must be an object")
    return document


__all__ = ["GuestFileError", "MAX_BYTES", "read_json_at"]
