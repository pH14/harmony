# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run one verified opaque artifact in a disposable container.

This module supplies functional execution plumbing for a fixed qualification
fixture.  It does not execute the artifact on the host and does not grade its
output.
"""

from __future__ import annotations

import hashlib
import os
import re
import stat
import tempfile
from dataclasses import dataclass
from pathlib import Path

try:
    from . import build, materials, sandbox
except ImportError:  # unittest discovery can load this directory as top-level.
    import build  # type: ignore[no-redef]
    import materials  # type: ignore[no-redef]
    import sandbox  # type: ignore[no-redef]


MAX_ARTIFACT_BYTES = 16 * 1024 * 1024
MAX_ARGS = 64
MAX_ARG_BYTES = 4096
MAX_ARG_TOTAL_BYTES = 64 * 1024
_HEX_RE = re.compile(r"^[0-9a-f]{64}$")
_PROGRAM_PATH = "/work/workspace/app"


@dataclass(frozen=True)
class ArtifactRun:
    """The immutable identity and bounded result of one artifact run."""

    artifact_sha256: str
    image: str
    argv: tuple[str, ...]
    result: sandbox.RunResult


def _validate_artifact(artifact: object) -> build.Artifact:
    if type(artifact) is not build.Artifact:
        raise ValueError("artifact must be a build.Artifact")
    try:
        path = build._output_path(artifact.path)
    except (TypeError, UnicodeError, ValueError) as exc:
        raise ValueError("artifact path must be a normalized relative POSIX path") from exc
    if not path:
        raise ValueError("artifact path must be nonempty")
    if type(artifact.data) is not bytes:
        raise ValueError("artifact data must be bytes")
    if not 0 < len(artifact.data) <= MAX_ARTIFACT_BYTES:
        raise ValueError("artifact data must be between 1 byte and 16 MiB")
    if type(artifact.sha256) is not str or _HEX_RE.fullmatch(artifact.sha256) is None:
        raise ValueError("artifact digest must be lowercase hexadecimal")
    if hashlib.sha256(artifact.data).hexdigest() != artifact.sha256:
        raise ValueError("artifact digest does not match artifact data")
    if artifact.executable is not True:
        raise ValueError("artifact must be executable")
    return artifact


def _validate_argv(argv: object) -> tuple[str, ...]:
    if type(argv) is not list:
        raise ValueError("argv must be a list of user arguments")
    if len(argv) > MAX_ARGS:
        raise ValueError("argv contains too many arguments")
    encoded: list[bytes] = []
    for argument in argv:
        if type(argument) is not str or "\x00" in argument:
            raise ValueError("argv arguments must be NUL-free strings")
        try:
            data = argument.encode("utf-8")
        except UnicodeEncodeError as exc:
            raise ValueError("argv arguments must be valid UTF-8") from exc
        if len(data) > MAX_ARG_BYTES:
            raise ValueError("argv argument exceeds 4096 UTF-8 bytes")
        encoded.append(data)
    if sum(map(len, encoded)) > MAX_ARG_TOTAL_BYTES:
        raise ValueError("argv arguments exceed the 64 KiB total bound")
    return tuple(argv)


def _validate_request(
    artifact: object,
    image: object,
    argv: object,
    limits: object,
) -> tuple[build.Artifact, str, tuple[str, ...], sandbox.Limits]:
    checked_artifact = _validate_artifact(artifact)
    try:
        checked_image = sandbox._validate_image(image)
    except (TypeError, ValueError) as exc:
        raise ValueError("image must be an immutable sha256:<64 lowercase hex> ID") from exc
    checked_argv = _validate_argv(argv)
    if not isinstance(limits, sandbox.Limits):
        raise ValueError("limits must be a sandbox.Limits instance")
    if limits.toolcalls < 1:
        raise ValueError("limits.toolcalls must allow one artifact run")
    return checked_artifact, checked_image, checked_argv, limits


def _source_signature(path: Path) -> tuple[int, int, int, int, int, int]:
    try:
        info = os.lstat(path)
    except OSError as exc:
        raise sandbox.SandboxError("artifact source cannot be inspected after the run") from exc
    if not stat.S_ISREG(info.st_mode):
        raise sandbox.SandboxError("artifact source is no longer a regular file")
    return (
        info.st_dev,
        info.st_ino,
        info.st_mode,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
    )


def _check_source(path: Path, artifact: build.Artifact, before: tuple[int, ...]) -> None:
    after = _source_signature(path)
    if after != before:
        raise sandbox.SandboxError("artifact source changed during the run")
    try:
        data = path.read_bytes()
    except OSError as exc:
        raise sandbox.SandboxError("artifact source cannot be read after the run") from exc
    if data != artifact.data or hashlib.sha256(data).hexdigest() != artifact.sha256:
        raise sandbox.SandboxError("artifact source bytes changed during the run")


def run_artifact(
    artifact: build.Artifact,
    image: str,
    argv: list[str],
    limits: sandbox.Limits,
) -> ArtifactRun:
    """Run one artifact under a fresh sandbox and return its bounded result.

    The artifact metadata path is validated for provenance but the container path
    is always /work/workspace/app. A sandbox and frozen pair are created for
    every call; no container or staged input is reused.
    """

    checked_artifact, checked_image, checked_argv, checked_limits = _validate_request(
        artifact, image, argv, limits
    )
    with tempfile.TemporaryDirectory(prefix="harmony-artifact-run-") as temporary:
        root = Path(temporary).resolve(strict=True)
        source = root / "app"
        try:
            source.write_bytes(checked_artifact.data)
            source.chmod(0o755)
        except OSError as exc:
            raise sandbox.SandboxError("artifact source cannot be staged") from exc
        source_before = _source_signature(source)

        pair = root / "pair"
        try:
            materials.freeze_pair(
                pair,
                {"app": source},
                {},
                "Run the supplied artifact in the isolated container.",
                {
                    "artifact": checked_artifact.sha256,
                    "image": checked_image,
                    "argv": list(checked_argv),
                },
            )
            manifest = hashlib.sha256((pair / "manifest.json").read_bytes()).hexdigest()
        except (OSError, materials.MaterialError) as exc:
            raise sandbox.SandboxError("artifact source pair cannot be frozen") from exc

        with sandbox.Sandbox(checked_image, checked_limits) as isolated:
            isolated.stage(pair, "docs", manifest)
            result = isolated.run([_PROGRAM_PATH, *checked_argv])

        _check_source(source, checked_artifact, source_before)
        try:
            materials.verify_pair(pair)
        except (OSError, materials.MaterialError) as exc:
            raise sandbox.SandboxError("artifact source pair changed during the run") from exc

    return ArtifactRun(checked_artifact.sha256, checked_image, checked_argv, result)


__all__ = ["ArtifactRun", "run_artifact"]
