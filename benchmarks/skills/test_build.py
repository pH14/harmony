# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import base64
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from collections.abc import Iterable
from pathlib import Path
from unittest import mock

try:
    from . import build, materials, sandbox
except ImportError:  # unittest discovery can load this directory as top-level.
    import build  # type: ignore[no-redef]
    import materials  # type: ignore[no-redef]
    import sandbox  # type: ignore[no-redef]


IMAGE = "sha256:" + "a" * 64
MAX_ARTIFACT_BYTES = 64


def _limits(*, outputbytes: int = 256 * 1024, toolcalls: int = 2) -> sandbox.Limits:
    return sandbox.Limits(
        wall=10.0,
        toolcalls=toolcalls,
        memorybytes=32 * 1024 * 1024,
        workbytes=8 * 1024 * 1024,
        cpus=0.5,
        pids=32,
        outputbytes=outputbytes,
    )


def _pair(root: Path) -> tuple[Path, str]:
    source = root / "source.c"
    source.write_bytes(b"int main(void) { return 0; }\n")
    pair = root / "pair"
    materials.freeze_pair(
        pair,
        {"main.c": source},
        {},
        "Compile the supplied program.",
        {"test": True},
    )
    manifest = hashlib.sha256((pair / "manifest.json").read_bytes()).hexdigest()
    return pair, manifest


def _run_result(
    exit_code: int | None,
    stdout: bytes = b"",
    stderr: bytes = b"",
    termination: str = "exited",
) -> sandbox.RunResult:
    return sandbox.RunResult(exit_code, stdout, stderr, termination)


class FakeSandbox:
    def __init__(
        self,
        results: Iterable[sandbox.RunResult],
        *,
        exit_error: BaseException | None = None,
        stage_error: BaseException | None = None,
    ) -> None:
        self.results = iter(results)
        self.exit_error = exit_error
        self.stage_error = stage_error
        self.staged: tuple[Path, str, str] | None = None
        self.runs: list[list[str]] = []

    def __enter__(self) -> "FakeSandbox":
        return self

    def __exit__(self, *_: object) -> None:
        if self.exit_error is not None:
            raise self.exit_error

    def stage(self, pair: Path, arm: str, manifest_sha256: str) -> None:
        if self.stage_error is not None:
            raise self.stage_error
        self.staged = (pair, arm, manifest_sha256)

    def run(self, argv: list[str]) -> sandbox.RunResult:
        self.runs.append(argv)
        return next(self.results)


class LocalCollectorTests(unittest.TestCase):
    def run_collector(
        self, root: Path, outputs: list[str], maximum: int
    ) -> subprocess.CompletedProcess[bytes]:
        # The production collector has one fixed root literal.  Replacing that
        # literal is the only fixture-specific change; the collector itself is
        # run in a local subprocess, without claiming a Linux/Docker boundary.
        marker = 'ROOT = "/work/workspace"'
        self.assertEqual(build._COLLECT_SCRIPT.count(marker), 1)
        script = build._COLLECT_SCRIPT.replace(marker, f"ROOT = {str(root)!r}", 1)
        return subprocess.run(
            [
                sys.executable,
                "-I",
                "-S",
                "-c",
                script,
                json.dumps(outputs, separators=(",", ":")),
                str(maximum),
            ],
            cwd=root,
            env={"PATH": "/usr/bin:/bin", "LC_ALL": "C.UTF-8"},
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=5.0,
        )

    def test_collector_copies_bytes_and_executable_modes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            executable = root / "app"
            executable.write_bytes(b"#!/bin/sh\nexit 0\n")
            os.chmod(executable, 0o711)
            nested = root / "bin"
            nested.mkdir()
            data = nested / "tool"
            data.write_bytes(b"opaque\x00bytes")
            os.chmod(data, 0o644)

            completed = self.run_collector(root, ["app", "bin/tool"], 64)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            decoded = build._decode_collector(
                _run_result(0, completed.stdout, completed.stderr),
                ("app", "bin/tool"),
                64,
            )
            self.assertEqual(decoded[0].path, "app")
            self.assertEqual(decoded[0].data, b"#!/bin/sh\nexit 0\n")
            self.assertTrue(decoded[0].executable)
            self.assertEqual(decoded[0].sha256, hashlib.sha256(decoded[0].data).hexdigest())
            self.assertEqual(decoded[1].path, "bin/tool")
            self.assertEqual(decoded[1].data, b"opaque\x00bytes")
            self.assertFalse(decoded[1].executable)

    def test_collector_enforces_total_artifact_byte_cap(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / "first").write_bytes(b"12345")
            (root / "second").write_bytes(b"67890")
            completed = self.run_collector(root, ["first", "second"], 8)
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn(b"artifact byte limit exceeded", completed.stderr)

    def test_collector_metadata_bound_allows_maximal_control_character_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            controls = "".join(chr(1 + (index % 31)) for index in range(120))
            components = (
                "component-a-" + controls,
                "component-b-" + controls,
                "component-c-" + controls,
            )
            directory = root.joinpath("nested", *components)
            directory.mkdir(parents=True)
            outputs: list[str] = []
            for index in range(64):
                relative = "/".join(("nested", *components, f"output-{index:02d}"))
                (root / relative).write_bytes(bytes((index,)))
                outputs.append(relative)

            completed = self.run_collector(root, outputs, 64)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertGreater(len(completed.stdout), 64 * 1024)
            decoded = build._decode_collector(
                _run_result(0, completed.stdout, completed.stderr),
                tuple(outputs),
                64,
            )
            self.assertEqual(len(decoded), 64)
            self.assertEqual([artifact.data for artifact in decoded], [bytes((i,)) for i in range(64)])

    def test_collector_rejects_unsafe_or_nonregular_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / "file").write_bytes(b"file")
            (root / "directory").mkdir()
            real = root / "real"
            real.mkdir()
            (real / "file").write_bytes(b"nested")
            os.symlink("file", root / "symlink")
            os.symlink("real", root / "intermediate")
            if not hasattr(os, "mkfifo"):
                self.skipTest("FIFO creation is unavailable on this platform")
            os.mkfifo(root / "fifo")
            cases = (
                ["/absolute"],
                ["../file"],
                ["directory/../file"],
                ["missing"],
                ["directory"],
                ["symlink"],
                ["intermediate/file"],
                ["fifo"],
            )
            for outputs in cases:
                with self.subTest(outputs=outputs):
                    completed = self.run_collector(root, list(outputs), 64)
                    self.assertNotEqual(completed.returncode, 0)


class BuildBoundaryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.pair, self.manifest = _pair(self.root)
        self.command = ["/bin/true"]
        self.outputs = ["app"]

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def build_with_fake(
        self,
        fake: FakeSandbox,
        *,
        outputs: list[str] | None = None,
        limits: sandbox.Limits | None = None,
        maximum: int = MAX_ARTIFACT_BYTES,
    ) -> build.BuildResult:
        with mock.patch.object(build.sandbox, "Sandbox", return_value=fake):
            return build.build_frozen(
                self.pair,
                self.manifest,
                IMAGE,
                self.command,
                self.outputs if outputs is None else outputs,
                _limits() if limits is None else limits,
                maximum,
            )

    def collector_result(self, data: bytes = b"artifact") -> sandbox.RunResult:
        encoded = base64.b64encode(data).decode("ascii")
        payload = json.dumps(
            {
                "version": 1,
                "artifacts": [
                    {"path": "app", "data": encoded, "executable": True}
                ],
            },
            separators=(",", ":"),
        ).encode("utf-8")
        return _run_result(0, payload)

    def test_request_validation_rejects_before_sandbox_creation(self) -> None:
        invalid = (
            {"manifest_sha256": "bad"},
            {"image": "latest"},
            {"argv": ["relative"]},
            {"argv": ["/bin/true", "\x00"]},
            {"outputs": ["../app"]},
            {"outputs": ["app", "app"]},
            {"outputs": ["/app"]},
            {"maximum": 0},
            {"limits": _limits(toolcalls=1)},
            {"limits": _limits(outputbytes=1)},
        )
        with mock.patch.object(build.sandbox, "Sandbox") as constructor:
            for changes in invalid:
                kwargs = {
                    "manifest_sha256": self.manifest,
                    "image": IMAGE,
                    "argv": self.command,
                    "outputs": self.outputs,
                    "limits": _limits(),
                    "maximum": MAX_ARTIFACT_BYTES,
                }
                if "manifest_sha256" in changes:
                    kwargs["manifest_sha256"] = changes["manifest_sha256"]
                if "image" in changes:
                    kwargs["image"] = changes["image"]
                if "argv" in changes:
                    kwargs["argv"] = changes["argv"]
                if "outputs" in changes:
                    kwargs["outputs"] = changes["outputs"]
                if "maximum" in changes:
                    kwargs["maximum"] = changes["maximum"]
                if "limits" in changes:
                    kwargs["limits"] = changes["limits"]
                with self.subTest(changes=changes):
                    with self.assertRaises(build.BuildError) as raised:
                        build.build_frozen(
                            self.pair,
                            kwargs["manifest_sha256"],
                            kwargs["image"],
                            kwargs["argv"],
                            kwargs["outputs"],
                            kwargs["limits"],
                            kwargs["maximum"],
                        )
                    self.assertEqual(raised.exception.phase, "validate")
                    self.assertIsNone(raised.exception.result)
            constructor.assert_not_called()

    def test_success_collects_after_build_and_returns_receipt(self) -> None:
        fake = FakeSandbox([_run_result(0, b"built\n"), self.collector_result()])
        result = self.build_with_fake(fake)
        self.assertEqual(fake.staged, (self.pair, "docs", self.manifest))
        self.assertEqual(len(fake.runs), 2)
        self.assertEqual(fake.runs[0], self.command)
        self.assertEqual(result.manifest_sha256, self.manifest)
        self.assertEqual(result.image, IMAGE)
        self.assertEqual(result.argv, tuple(self.command))
        self.assertEqual(result.stdout, b"built\n")
        self.assertEqual(result.stderr, b"")
        self.assertEqual(result.artifacts[0].data, b"artifact")

    def test_failed_build_does_not_invoke_collection_or_return_artifacts(self) -> None:
        failed = _run_result(2, b"", b"compiler failed")
        fake = FakeSandbox([failed, self.collector_result()])
        with self.assertRaises(build.BuildError) as raised:
            self.build_with_fake(fake)
        self.assertEqual(raised.exception.phase, "build")
        self.assertIs(raised.exception.result, failed)
        self.assertEqual(len(fake.runs), 1)

    def test_failed_collection_reports_the_collector_result(self) -> None:
        failed = _run_result(1, b"malformed", b"collector failed")
        fake = FakeSandbox([_run_result(0), failed])
        with self.assertRaises(build.BuildError) as raised:
            self.build_with_fake(fake)
        self.assertEqual(raised.exception.phase, "collect")
        self.assertIs(raised.exception.result, failed)
        self.assertEqual(len(fake.runs), 2)

    def test_sandbox_errors_propagate_without_becoming_build_errors(self) -> None:
        infrastructure = sandbox.SandboxError("docker unavailable")
        fake = FakeSandbox([], stage_error=infrastructure)
        with self.assertRaises(sandbox.SandboxError) as raised:
            self.build_with_fake(fake)
        self.assertIs(raised.exception, infrastructure)

    def test_cleanup_failure_cannot_return_a_successful_build(self) -> None:
        cleanup = sandbox.SandboxError("cleanup failed")
        fake = FakeSandbox(
            [_run_result(0, b"built"), self.collector_result()], exit_error=cleanup
        )
        with self.assertRaises(sandbox.SandboxError) as raised:
            self.build_with_fake(fake)
        self.assertIs(raised.exception, cleanup)


class CollectorDecoderTests(unittest.TestCase):
    def decode(self, payload: bytes, outputs: tuple[str, ...] = ("app",)) -> build.BuildError:
        result = _run_result(0, payload)
        with self.assertRaises(build.BuildError) as raised:
            build._decode_collector(result, outputs, MAX_ARTIFACT_BYTES)
        self.assertEqual(raised.exception.phase, "collect")
        self.assertIs(raised.exception.result, result)
        return raised.exception

    def test_host_decoder_rejects_malformed_collector_documents(self) -> None:
        valid_record = {"path": "app", "data": "YQ==", "executable": False}
        cases = (
            b'{"version":1,"version":1,"artifacts":[]}',
            json.dumps({"version": 2, "artifacts": []}).encode(),
            json.dumps({"version": 1, "artifacts": [{"path": "other", "data": "YQ==", "executable": False}]}).encode(),
            json.dumps({"version": 1, "artifacts": [{"path": "app", "data": "%%%", "executable": False}]}).encode(),
            json.dumps({"version": 1, "artifacts": [{"path": "app", "data": "YQ", "executable": False}]}).encode(),
            json.dumps({"version": 1, "artifacts": [{"path": "app", "data": "YQ==", "executable": 1}]}).encode(),
            json.dumps({"version": 1, "artifacts": [{**valid_record, "extra": True}]}).encode(),
        )
        for payload in cases:
            with self.subTest(payload=payload):
                self.decode(payload)

    def test_host_decoder_rejects_total_size_over_cap(self) -> None:
        payload = json.dumps(
            {
                "version": 1,
                "artifacts": [
                    {"path": "app", "data": base64.b64encode(b"x" * 65).decode(), "executable": False}
                ],
            }
        ).encode()
        self.decode(payload)


if __name__ == "__main__":
    unittest.main()
