# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import hashlib
import unittest
from pathlib import Path
from typing import Iterable
from unittest import mock

try:
    from . import artifact_run, build, materials, sandbox
except ImportError:  # unittest discovery can load this directory as top-level.
    import artifact_run  # type: ignore[no-redef]
    import build  # type: ignore[no-redef]
    import materials  # type: ignore[no-redef]
    import sandbox  # type: ignore[no-redef]


IMAGE = "sha256:" + "a" * 64
MAX_ARTIFACT_BYTES = 16 * 1024 * 1024
MAX_ARGS = 64
MAX_ARG_BYTES = 4096
MAX_ARG_TOTAL_BYTES = 64 * 1024


def _limits() -> sandbox.Limits:
    return sandbox.Limits(
        wall=10.0,
        toolcalls=1,
        memorybytes=32 * 1024 * 1024,
        workbytes=32 * 1024 * 1024,
        cpus=0.5,
        pids=32,
        outputbytes=64 * 1024,
    )


def _artifact(
    data: object = b"artifact",
    *,
    path: str = "app",
    executable: bool = True,
    sha256: str | None = None,
) -> build.Artifact:
    digest = sha256
    if digest is None and isinstance(data, bytes):
        digest = hashlib.sha256(data).hexdigest()
    return build.Artifact(path, data, "0" * 64 if digest is None else digest, executable)  # type: ignore[arg-type]


def _result(
    exit_code: int | None,
    stdout: bytes = b"",
    stderr: bytes = b"",
    termination: str = "exited",
) -> sandbox.RunResult:
    return sandbox.RunResult(exit_code, stdout, stderr, termination)


class FakeSandbox:
    """A public-boundary double that still reads the real frozen pair."""

    def __init__(
        self,
        result: sandbox.RunResult,
        *,
        exit_error: BaseException | None = None,
    ) -> None:
        self.result = result
        self.exit_error = exit_error
        self.entered = False
        self.closed = False
        self.staged: tuple[Path, str, str] | None = None
        self.staged_bytes: bytes | None = None
        self.runs: list[tuple[str, ...]] = []

    def __enter__(self) -> "FakeSandbox":
        self.entered = True
        return self

    def __exit__(self, *_: object) -> bool:
        self.closed = True
        if self.exit_error is not None:
            raise self.exit_error
        return False

    def stage(self, pair: Path, arm: str, manifest_sha256: str) -> None:
        if self.staged is not None:
            raise AssertionError("one sandbox was reused for multiple artifact runs")
        materials.verify_pair(pair)
        selected = pair / arm
        app = selected / "app"
        if not app.is_file():
            raise AssertionError(f"selected arm did not contain fixed app: {app}")
        expected_manifest = hashlib.sha256((pair / "manifest.json").read_bytes()).hexdigest()
        if manifest_sha256 != expected_manifest:
            raise AssertionError("sandbox received the wrong frozen-pair manifest")
        self.staged = (pair, arm, manifest_sha256)
        self.staged_bytes = app.read_bytes()

    def run(self, argv: list[str]) -> sandbox.RunResult:
        self.runs.append(tuple(argv))
        return self.result


class SandboxFactory:
    def __init__(
        self,
        results: Iterable[sandbox.RunResult],
        *,
        exit_errors: Iterable[BaseException | None] | None = None,
    ) -> None:
        self.results = iter(results)
        self.exit_errors = iter(exit_errors or ())
        self.instances: list[FakeSandbox] = []
        self.constructor_args: list[tuple[str, sandbox.Limits]] = []

    def __call__(self, *args: object, **kwargs: object) -> FakeSandbox:
        image = kwargs.get("image", args[0] if args else None)
        limits = kwargs.get("limits", args[1] if len(args) > 1 else None)
        if not isinstance(image, str) or not isinstance(limits, sandbox.Limits):
            raise AssertionError("unexpected Sandbox constructor shape")
        fake = FakeSandbox(next(self.results), exit_error=next(self.exit_errors, None))
        self.constructor_args.append((image, limits))
        self.instances.append(fake)
        return fake


class ArtifactRunTests(unittest.TestCase):
    def run_artifact(
        self,
        artifact: build.Artifact,
        args: list[str],
        factory: SandboxFactory,
        *,
        image: str = IMAGE,
        limits: sandbox.Limits | None = None,
    ) -> artifact_run.ArtifactRun:
        with mock.patch.object(artifact_run.sandbox, "Sandbox", side_effect=factory):
            return artifact_run.run_artifact(
                artifact,
                image,
                args,
                _limits() if limits is None else limits,
            )

    def test_success_uses_fresh_sandboxes_fixed_app_and_exact_args(self) -> None:
        first_data = b"#!/bin/sh\nprintf host-data\n"
        second_data = b"second opaque bytes\x00"
        first_result = _result(0, b"first output")
        second_result = _result(0, b"second output")
        factory = SandboxFactory([first_result, second_result])

        first = self.run_artifact(_artifact(first_data), ["--first", "value"], factory)
        second = self.run_artifact(_artifact(second_data), ["--second"], factory)

        self.assertEqual(len(factory.instances), 2)
        self.assertIsNot(factory.instances[0], factory.instances[1])
        self.assertEqual(factory.constructor_args, [(IMAGE, _limits()), (IMAGE, _limits())])
        for instance, expected_data in zip(factory.instances, (first_data, second_data)):
            self.assertTrue(instance.entered)
            self.assertTrue(instance.closed)
            self.assertIsNotNone(instance.staged)
            self.assertEqual(instance.staged_bytes, expected_data)
            self.assertEqual(len(instance.runs), 1)
            self.assertEqual(instance.runs[0][0], "/work/workspace/app")

        self.assertEqual(factory.instances[0].runs[0], ("/work/workspace/app", "--first", "value"))
        self.assertEqual(factory.instances[1].runs[0], ("/work/workspace/app", "--second"))
        self.assertEqual(first.artifact_sha256, hashlib.sha256(first_data).hexdigest())
        self.assertEqual(second.artifact_sha256, hashlib.sha256(second_data).hexdigest())
        self.assertEqual(first.image, IMAGE)
        self.assertEqual(second.image, IMAGE)
        self.assertEqual(first.argv, ("--first", "value"))
        self.assertEqual(second.argv, ("--second",))
        self.assertIs(first.result, first_result)
        self.assertIs(second.result, second_result)

    def test_nonzero_and_timeout_results_are_returned_for_caller_classification(self) -> None:
        cases = (
            _result(7, stderr=b"failed"),
            _result(None, stderr=b"deadline", termination="timeout"),
        )
        for command_result in cases:
            with self.subTest(command_result=command_result):
                factory = SandboxFactory([command_result])
                receipt = self.run_artifact(_artifact(), ["--case"], factory)
                self.assertIs(receipt.result, command_result)
                self.assertEqual(receipt.argv, ("--case",))
                self.assertEqual(len(factory.instances), 1)
                self.assertEqual(factory.instances[0].runs, [("/work/workspace/app", "--case")])
                self.assertTrue(factory.instances[0].closed)

    def test_cleanup_failure_propagates_after_successful_execution(self) -> None:
        cleanup = sandbox.SandboxError("cleanup failed")
        factory = SandboxFactory([_result(0)], exit_errors=[cleanup])
        with self.assertRaises(sandbox.SandboxError) as raised:
            self.run_artifact(_artifact(), ["--case"], factory)
        self.assertIs(raised.exception, cleanup)
        self.assertTrue(factory.instances[0].closed)

    def test_validation_rejects_bad_artifacts_images_paths_and_args_before_sandbox(self) -> None:
        data = b"valid"
        oversized = b"x" * (MAX_ARTIFACT_BYTES + 1)
        invalid_artifacts = (
            ("hash mismatch", _artifact(data, sha256="0" * 64)),
            ("nonbytes", _artifact(bytearray(data))),
            ("nonbytes string", _artifact("text")),
            ("nonexecutable", _artifact(data, executable=False)),
            ("empty", _artifact(b"")),
            ("oversized", _artifact(oversized)),
        )
        invalid_paths = ("", ".", "../app", "/app", "app/../other", "app//child", "app\x00")
        invalid_args = (
            ("not a list", ("--flag",)),
            ("NUL", ["--ok", "bad\x00arg"]),
            ("too many", ["x"] * (MAX_ARGS + 1)),
            ("argument too long", ["x" * (MAX_ARG_BYTES + 1)]),
            (
                "total too long",
                ["x" * MAX_ARG_BYTES] * (MAX_ARG_TOTAL_BYTES // MAX_ARG_BYTES)
                + ["y"],
            ),
            ("nonstring", ["--ok", 1]),
        )
        cases: list[tuple[str, build.Artifact, str, object]] = [
            (name, artifact, IMAGE, ["--ok"])
            for name, artifact in invalid_artifacts
        ]
        cases.extend(
            (f"path {path!r}", _artifact(data, path=path), IMAGE, ["--ok"])
            for path in invalid_paths
        )
        cases.extend(
            (f"args {name}", _artifact(data), IMAGE, args)
            for name, args in invalid_args
        )
        cases.append(("invalid image", _artifact(data), "latest", ["--ok"]))

        factory = SandboxFactory([])
        with mock.patch.object(artifact_run.sandbox, "Sandbox", side_effect=factory) as constructor:
            for name, artifact, image, args in cases:
                with self.subTest(name=name):
                    with self.assertRaises(Exception):
                        artifact_run.run_artifact(artifact, image, args, _limits())  # type: ignore[arg-type]
            constructor.assert_not_called()


if __name__ == "__main__":
    unittest.main()
