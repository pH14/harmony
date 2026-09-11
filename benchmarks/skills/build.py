# SPDX-License-Identifier: AGPL-3.0-or-later
"""Build a frozen submission inside the qualified Docker boundary.

This module is a bounded transport for build receipts.  It does not decide
whether a build is useful, whether its outputs came from its sources, or
whether a compiler behaved honestly; those are separate qualification
questions.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import stat
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

try:
    from . import sandbox
except ImportError:  # unittest discovery can load this directory as top-level.
    import sandbox  # type: ignore[no-redef]


MAX_ARTIFACT_BYTES = 16 * 1024 * 1024
MAX_OUTPUTS = 64
MAX_OUTPUT_PATH_BYTES = 512
# ``ensure_ascii=False`` still escapes C0 characters as six-byte ``\uXXXX``
# sequences.  Reserve that worst case for every accepted path, plus framing,
# per-file base64 padding, and the fixed collector envelope.
COLLECTOR_METADATA_BYTES = 6 * MAX_OUTPUT_PATH_BYTES * MAX_OUTPUTS + 4 * 1024
_IMAGE_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
_HEX_RE = re.compile(r"^[0-9a-f]{64}$")


class BuildError(RuntimeError):
    """A validation, completed-build, or completed-collection failure."""

    def __init__(
        self,
        phase: Literal["validate", "build", "collect"],
        message: str,
        result: sandbox.RunResult | None = None,
    ) -> None:
        self.phase = phase
        self.result = result
        super().__init__(f"{phase}: {message}")


@dataclass(frozen=True)
class Artifact:
    """One output copied from the container and hashed on the host."""

    path: str
    data: bytes
    sha256: str
    executable: bool


@dataclass(frozen=True)
class BuildResult:
    """A bounded build receipt with input and command provenance."""

    manifest_sha256: str
    image: str
    argv: tuple[str, ...]
    artifacts: tuple[Artifact, ...]
    stdout: bytes
    stderr: bytes


def _unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _output_path(value: object) -> str:
    if not isinstance(value, str) or not value or "\x00" in value or "\\" in value:
        raise ValueError("output paths must be normalized relative POSIX paths")
    try:
        length = len(value.encode("utf-8"))
    except UnicodeEncodeError as exc:
        raise ValueError("output paths must be valid UTF-8") from exc
    if length > MAX_OUTPUT_PATH_BYTES or value.startswith("/"):
        raise ValueError("output path is absolute or too long")
    parts = value.split("/")
    if any(not part or part in {".", ".."} for part in parts):
        raise ValueError("output path is not normalized")
    if "/".join(parts) != value:
        raise ValueError("output path is not normalized")
    return value


def _validate_outputs(value: object) -> tuple[str, ...]:
    if not isinstance(value, list) or not 1 <= len(value) <= MAX_OUTPUTS:
        raise ValueError("outputs must contain between 1 and 64 paths")
    paths: list[str] = []
    seen: set[str] = set()
    for item in value:
        path = _output_path(item)
        if path in seen:
            raise ValueError("outputs must be unique")
        seen.add(path)
        paths.append(path)
    return tuple(paths)


def _collector_output_bound(maximum: int) -> int:
    encoded = ((maximum + 2) // 3) * 4
    return encoded + COLLECTOR_METADATA_BYTES


def _validate_request(
    pair: Path,
    manifest_sha256: str,
    image: str,
    argv: list[str],
    outputs: list[str],
    limits: sandbox.Limits,
    max_artifact_bytes: int,
) -> tuple[Path, str, str, tuple[str, ...], tuple[str, ...], int]:
    try:
        pair_path = Path(pair)
        if not _HEX_RE.fullmatch(manifest_sha256):
            raise ValueError("manifest_sha256 must be 64 lowercase hex characters")
        if not _IMAGE_RE.fullmatch(image):
            raise ValueError("image must be an immutable sha256:<64 lowercase hex> ID")
        if not isinstance(argv, list) or not argv:
            raise ValueError("argv must be a nonempty list")
        if any(not isinstance(item, str) or "\x00" in item for item in argv):
            raise ValueError("argv entries must be NUL-free strings")
        if not os.path.isabs(argv[0]):
            raise ValueError("argv[0] must be an absolute program path")
        if not isinstance(limits, sandbox.Limits):
            raise ValueError("limits must be a sandbox.Limits instance")
        if limits.toolcalls < 2:
            raise ValueError("limits.toolcalls must allow build and collection")
        if type(max_artifact_bytes) is not int or not 1 <= max_artifact_bytes <= MAX_ARTIFACT_BYTES:
            raise ValueError("max_artifact_bytes must be between 1 and 16 MiB")
        if limits.outputbytes < _collector_output_bound(max_artifact_bytes):
            raise ValueError("limits.outputbytes is too small for collection")
        output_paths = _validate_outputs(outputs)
    except (TypeError, ValueError, UnicodeError) as exc:
        raise BuildError("validate", str(exc)) from exc
    return pair_path, manifest_sha256, image, tuple(argv), output_paths, max_artifact_bytes


# The root is fixed in production.  Portable tests replace this one literal
# with a temporary directory; requested paths and the byte cap remain argv[1:3].
_COLLECT_SCRIPT = r'''import base64,json,os,stat,sys

ROOT = "/work/workspace"
MAX_PATH_BYTES = 512
MAX_OUTPUTS = 64
MAX_BYTES = 16 * 1024 * 1024

def fail(message):
    sys.stderr.write("collector failed: " + message + "\n")
    raise SystemExit(1)

def output_path(value):
    if not isinstance(value, str) or not value or "\x00" in value or "\\" in value:
        fail("output paths must be normalized relative POSIX paths")
    try:
        length = len(value.encode("utf-8"))
    except UnicodeEncodeError:
        fail("output path is not valid UTF-8")
    if length > MAX_PATH_BYTES or value.startswith("/"):
        fail("output path is absolute or too long")
    parts = value.split("/")
    if any(not part or part in (".", "..") for part in parts):
        fail("output path is not normalized")
    if "/".join(parts) != value:
        fail("output path is not normalized")
    return parts

if len(sys.argv) != 3:
    fail("invalid collector arguments")
try:
    requested = json.loads(sys.argv[1])
    cap_text = sys.argv[2]
    if not cap_text.isascii() or not cap_text.isdecimal():
        fail("invalid byte cap")
    cap = int(cap_text)
except (UnicodeError, ValueError, json.JSONDecodeError):
    fail("invalid collector arguments")
if not isinstance(requested, list) or not 1 <= len(requested) <= MAX_OUTPUTS:
    fail("invalid output allowlist")
seen = set()
parts_by_name = {}
for value in requested:
    parts = output_path(value)
    if value in seen:
        fail("duplicate output path")
    seen.add(value)
    parts_by_name[value] = parts
if not 1 <= cap <= MAX_BYTES:
    fail("invalid byte cap")

directory_flag = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
file_flag = os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW
try:
    root_fd = os.open(ROOT, directory_flag)
except OSError:
    fail("workspace is not a directory")

def read_output(name, parts, remaining):
    directories = []
    current = root_fd
    descriptor = None
    try:
        for component in parts[:-1]:
            descriptor = os.open(component, directory_flag, dir_fd=current)
            directories.append(descriptor)
            current = descriptor
            descriptor = None
        descriptor = os.open(parts[-1], file_flag, dir_fd=current)
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode):
            fail("output is not a regular file")
        data = bytearray()
        while len(data) <= remaining:
            chunk = os.read(descriptor, min(64 * 1024, remaining + 1 - len(data)))
            if not chunk:
                break
            data.extend(chunk)
            if len(data) > remaining:
                fail("artifact byte limit exceeded")
        return bytes(data), bool(info.st_mode & 0o111)
    except OSError:
        fail("output cannot be opened or read")
    finally:
        if descriptor is not None:
            os.close(descriptor)
        for directory in reversed(directories):
            os.close(directory)

records = []
total = 0
try:
    for name in requested:
        data, executable = read_output(name, parts_by_name[name], cap - total)
        total += len(data)
        if total > cap:
            fail("artifact byte limit exceeded")
        records.append({"path": name, "data": base64.b64encode(data).decode("ascii"), "executable": executable})
finally:
    os.close(root_fd)
payload = json.dumps({"version": 1, "artifacts": records}, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
sys.stdout.buffer.write(payload)
'''


def _collector_argv(outputs: tuple[str, ...], maximum: int) -> list[str]:
    requested = json.dumps(list(outputs), ensure_ascii=False, separators=(",", ":"))
    return [
        "/usr/bin/python3",
        "-I",
        "-S",
        "-c",
        _COLLECT_SCRIPT,
        requested,
        str(maximum),
    ]


def _decode_collector(
    result: sandbox.RunResult,
    outputs: tuple[str, ...],
    maximum: int,
) -> tuple[Artifact, ...]:
    if result.termination != "exited" or result.exit_code != 0:
        raise BuildError("collect", "collector did not exit successfully", result)
    if len(result.stdout) > _collector_output_bound(maximum):
        raise BuildError("collect", "collector output exceeded its bound", result)
    try:
        document = json.loads(result.stdout.decode("utf-8"), object_pairs_hook=_unique_object)
        if not isinstance(document, dict) or set(document) != {"version", "artifacts"}:
            raise ValueError("collector schema is invalid")
        if type(document["version"]) is not int or document["version"] != 1:
            raise ValueError("collector version is invalid")
        records = document["artifacts"]
        if not isinstance(records, list) or len(records) != len(outputs):
            raise ValueError("collector output set is invalid")
        expected = set(outputs)
        found: dict[str, Artifact] = {}
        total = 0
        for record in records:
            if not isinstance(record, dict) or set(record) != {"path", "data", "executable"}:
                raise ValueError("collector artifact schema is invalid")
            path = _output_path(record["path"])
            if path not in expected or path in found:
                raise ValueError("collector output set is invalid")
            encoded = record["data"]
            if not isinstance(encoded, str):
                raise ValueError("collector data is not base64 text")
            if len(encoded) > ((maximum + 2) // 3) * 4:
                raise ValueError("collector artifact exceeds its bound")
            try:
                data = base64.b64decode(encoded.encode("ascii"), validate=True)
            except (UnicodeEncodeError, ValueError) as exc:
                raise ValueError("collector data is not valid base64") from exc
            if base64.b64encode(data).decode("ascii") != encoded:
                raise ValueError("collector data is not canonical base64")
            executable = record["executable"]
            if type(executable) is not bool:
                raise ValueError("collector executable flag is invalid")
            total += len(data)
            if total > maximum:
                raise ValueError("collector artifacts exceed their bound")
            found[path] = Artifact(path, data, hashlib.sha256(data).hexdigest(), executable)
        if set(found) != expected:
            raise ValueError("collector output set is invalid")
    except (UnicodeDecodeError, json.JSONDecodeError, TypeError, ValueError) as exc:
        raise BuildError("collect", str(exc), result) from exc
    return tuple(found[path] for path in outputs)


def build_frozen(
    pair: Path,
    manifest_sha256: str,
    image: str,
    argv: list[str],
    outputs: list[str],
    limits: sandbox.Limits,
    max_artifact_bytes: int,
) -> BuildResult:
    """Build and collect allowlisted outputs in one disposable sandbox."""
    pair_path, manifest, pinned_image, command, output_paths, maximum = _validate_request(
        pair, manifest_sha256, image, argv, outputs, limits, max_artifact_bytes
    )
    with sandbox.Sandbox(pinned_image, limits) as isolated:
        isolated.stage(pair_path, "docs", manifest)
        build_result = isolated.run(list(command))
        if build_result.termination != "exited" or build_result.exit_code != 0:
            raise BuildError("build", "build command did not exit successfully", build_result)
        collected = isolated.run(_collector_argv(output_paths, maximum))
        artifacts = _decode_collector(collected, output_paths, maximum)
    return BuildResult(manifest, pinned_image, command, artifacts, build_result.stdout, build_result.stderr)


__all__ = ["Artifact", "BuildError", "BuildResult", "build_frozen"]
