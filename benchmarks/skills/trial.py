# SPDX-License-Identifier: AGPL-3.0-or-later
"""Capture one bounded agent trial and its selected source outputs.

The trial owns only the lifecycle around a pre-frozen material pair.  An agent
may edit the selected arm in a fresh sandbox; the final collection command is
controller-owned and copies only the output paths fixed by the caller.
"""

from __future__ import annotations

import hashlib
import json
import threading
from contextlib import ExitStack
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

try:
    from . import build, materials, sandbox
except ImportError:  # unittest discovery can load this directory as top-level.
    import build  # type: ignore[no-redef]
    import materials  # type: ignore[no-redef]
    import sandbox  # type: ignore[no-redef]


_ARMS = frozenset({"docs", "skills"})
_RESERVED_MATERIAL_NAMES = frozenset({"TASK.txt", "SETTINGS.json", ".agent"})


@dataclass(frozen=True)
class Submission:
    """Immutable receipt of the selected source bytes and agent activity."""

    manifest_sha256: str
    arm: Literal["docs", "skills"]
    image: str
    sources: tuple[build.Artifact, ...]
    agent_toolcalls: int
    commands: tuple[tuple[str, ...], ...]
    results: tuple[sandbox.RunResult, ...]


def _manifest_digest(pair: Path) -> str:
    try:
        data = (pair / "manifest.json").read_bytes()
    except OSError as exc:
        raise sandbox.SandboxError("frozen pair manifest cannot be read") from exc
    return hashlib.sha256(data).hexdigest()


def _verify_pair(pair: Path, expected: str) -> None:
    try:
        materials.verify_pair(pair)
        first = _manifest_digest(pair)
        materials.verify_pair(pair)
        second = _manifest_digest(pair)
    except (OSError, materials.MaterialError) as exc:
        raise sandbox.SandboxError("frozen pair verification failed") from exc
    if first != second or first != expected:
        raise sandbox.SandboxError("frozen pair manifest hash does not match its anchor")


def _load_trial_settings(
    pair: Path,
    *,
    image: str,
    limits: sandbox.Limits,
    outputs: tuple[str, ...],
    maximum: int,
) -> None:
    try:
        raw = (pair / "docs" / "SETTINGS.json").read_bytes()
        settings = json.loads(raw.decode("utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise sandbox.SandboxError("frozen pair settings cannot be read") from exc
    if not isinstance(settings, dict):
        raise sandbox.SandboxError("frozen pair settings must be an object")
    expected = {
        "image": image,
        "limits": vars(limits).copy(),
        "outputs": list(outputs),
        "max_source_bytes": maximum,
    }
    if settings.get("trial") != expected:
        raise ValueError("frozen pair trial settings do not match the request")


def _validate_outputs(outputs: object) -> tuple[str, ...]:
    checked = build._validate_outputs(outputs)
    for path in checked:
        parts = path.split("/")
        if any(component in _RESERVED_MATERIAL_NAMES for component in parts):
            raise ValueError("trial outputs must not name reserved material paths")
    return checked


class TrialSession:
    """Run bounded agent requests, then collect a fixed source allowlist."""

    def __init__(
        self,
        pair: Path,
        manifest_sha256: str,
        arm: Literal["docs", "skills"],
        image: str,
        limits: sandbox.Limits,
        outputs: list[str],
        max_source_bytes: int,
    ) -> None:
        if not isinstance(pair, Path):
            raise ValueError("pair must be a pathlib.Path")
        if type(arm) is not str or arm not in _ARMS:
            raise ValueError("arm must be docs or skills")
        if not isinstance(limits, sandbox.Limits):
            raise ValueError("limits must be a sandbox.Limits instance")
        checked_image = sandbox._validate_image(image)
        checked_manifest = sandbox._validate_manifest_hash(manifest_sha256)
        if (
            type(max_source_bytes) is not int
            or not 1 <= max_source_bytes <= build.MAX_ARTIFACT_BYTES
        ):
            raise ValueError("max_source_bytes must be between 1 and 16 MiB")
        checked_outputs = _validate_outputs(outputs)
        if limits.toolcalls < 2:
            raise ValueError("limits.toolcalls must reserve a collection call")
        if limits.outputbytes < build._collector_output_bound(max_source_bytes):
            raise ValueError("limits.outputbytes is too small for source collection")

        _verify_pair(pair, checked_manifest)
        _load_trial_settings(
            pair,
            image=checked_image,
            limits=limits,
            outputs=checked_outputs,
            maximum=max_source_bytes,
        )

        self._pair = pair
        self._manifest_sha256 = checked_manifest
        self._arm: Literal["docs", "skills"] = arm
        self._image = checked_image
        self._limits = limits
        self._outputs = checked_outputs
        self._maximum = max_source_bytes
        self._lock = threading.RLock()
        self._stack = ExitStack()
        self._sandbox: sandbox.Sandbox | None = None
        self._closed = False
        self._stack_closed = False
        self._agent_toolcalls = 0
        self._commands: list[tuple[str, ...]] = []
        self._results: list[sandbox.RunResult] = []
        self._unusable_error: sandbox.SandboxError | None = None

        try:
            isolated = self._stack.enter_context(sandbox.Sandbox(checked_image, limits))
            self._sandbox = isolated
            isolated.stage(pair, arm, checked_manifest)
        except BaseException as original:
            try:
                self._stack.close()
            except BaseException as cleanup:
                raise cleanup from original
            raise

    def __enter__(self) -> TrialSession:
        with self._lock:
            self._ensure_active()
            return self

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> bool:
        self.close()
        return False

    def _ensure_active(self) -> sandbox.Sandbox:
        if self._closed or self._stack_closed or self._sandbox is None:
            raise sandbox.SandboxError("trial session is closed")
        return self._sandbox

    def _close_locked(self) -> None:
        if self._stack_closed:
            return
        self._closed = True
        try:
            self._stack.close()
        finally:
            self._stack_closed = True

    def close(self) -> None:
        """Destroy the trial sandbox; repeated close calls are harmless."""
        with self._lock:
            self._close_locked()

    def run(self, argv: list[str]) -> sandbox.RunResult:
        """Run one agent command and retain its result.

        Ordinary nonzero or signaled commands remain recoverable so the agent
        can correct them within the remaining budget; hard limits and sandbox
        failures make the session unusable for collection.
        """
        with self._lock:
            isolated = self._ensure_active()
            # Every caller attempt is accounted for, including requests that
            # cannot be dispatched.  The final collection call remains a
            # trusted slot in the underlying Sandbox budget.
            if self._agent_toolcalls >= self._limits.toolcalls - 1:
                self._agent_toolcalls += 1
                raise sandbox.SandboxError("trial agent tool-call budget exhausted")
            self._agent_toolcalls += 1
            sandbox._validate_argv(argv)
            command = tuple(argv)
            try:
                result = isolated.run(list(command))
            except ValueError:
                raise
            except sandbox.SandboxError as error:
                self._unusable_error = error
                raise
            if result.termination in {"timeout", "output_limit"}:
                self._unusable_error = sandbox.SandboxError(
                    f"agent command ended with hard limit {result.termination}"
                )
            self._commands.append(command)
            self._results.append(result)
            return result

    def finish(self) -> Submission:
        """Collect fixed outputs and close before returning a submission."""
        with self._lock:
            self._ensure_active()
            self._closed = True
            collection_error: BaseException | None = None
            sources: tuple[build.Artifact, ...] | None = None
            try:
                if self._unusable_error is not None:
                    raise self._unusable_error
                isolated = self._sandbox
                if isolated is None:
                    raise sandbox.SandboxError("trial session has no sandbox")
                result = isolated.run(build._collector_argv(self._outputs, self._maximum))
                sources = build._decode_collector(result, self._outputs, self._maximum)
            except BaseException as error:
                collection_error = error

            try:
                self._close_locked()
            except BaseException as cleanup:
                if collection_error is not None:
                    raise cleanup from collection_error
                raise
            if collection_error is not None:
                raise collection_error
            if sources is None:
                raise sandbox.SandboxError("source collection produced no submission")
            _verify_pair(self._pair, self._manifest_sha256)
            return Submission(
                manifest_sha256=self._manifest_sha256,
                arm=self._arm,
                image=self._image,
                sources=sources,
                agent_toolcalls=self._agent_toolcalls,
                commands=tuple(self._commands),
                results=tuple(self._results),
            )


__all__ = ["Submission", "TrialSession"]
