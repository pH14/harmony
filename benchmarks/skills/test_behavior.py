# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import hashlib
import unittest
from unittest import mock

try:
    from . import artifact_run, behavior, build, sandbox, semantic_cases
except ImportError:  # unittest discovery can load this directory as top-level.
    import artifact_run  # type: ignore[no-redef]
    import behavior  # type: ignore[no-redef]
    import build  # type: ignore[no-redef]
    import sandbox  # type: ignore[no-redef]
    import semantic_cases  # type: ignore[no-redef]


IMAGE = "sha256:" + "a" * 64


def _limits() -> sandbox.Limits:
    return sandbox.Limits(
        wall=10.0,
        toolcalls=64,
        memorybytes=32 * 1024 * 1024,
        workbytes=8 * 1024 * 1024,
        cpus=0.5,
        pids=32,
        outputbytes=64 * 1024,
    )


def _artifact() -> build.Artifact:
    data = b"controller-only artifact bytes"
    return build.Artifact("app", data, hashlib.sha256(data).hexdigest(), True)


def _result(
    stdout: bytes,
    *,
    exit_code: int | None = 0,
    stderr: bytes = b"",
    termination: str = "exited",
) -> sandbox.RunResult:
    return sandbox.RunResult(exit_code, stdout, stderr, termination)


def _receipt(
    artifact: build.Artifact,
    image: str,
    arguments: tuple[str, ...],
    result: sandbox.RunResult,
    *,
    artifact_sha256: str | None = None,
    receipt_image: str | None = None,
    receipt_argv: tuple[str, ...] | None = None,
) -> artifact_run.ArtifactRun:
    return artifact_run.ArtifactRun(
        artifact.sha256 if artifact_sha256 is None else artifact_sha256,
        image if receipt_image is None else receipt_image,
        arguments if receipt_argv is None else receipt_argv,
        result,
    )


class BehaviorGradeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.artifact = _artifact()
        self.limits = _limits()

    @staticmethod
    def _sample_by_arguments(arguments: list[str]) -> semantic_cases.Behavior:
        for case in semantic_cases.cases():
            for sample in case.behavior:
                if sample.arguments == tuple(arguments):
                    return sample
        raise AssertionError(f"unexpected private behavior arguments: {arguments!r}")

    def test_private_cases_run_to_completion_when_every_observation_matches(self) -> None:
        # These are the controller-private contracts.  Only their argument and
        # expected-value records reach this mocked loop; no source is staged or
        # executed locally.
        for case in semantic_cases.cases():
            with self.subTest(case=case.name):
                def matching(
                    artifact: build.Artifact,
                    image: str,
                    arguments: list[str],
                    limits: sandbox.Limits,
                ) -> artifact_run.ArtifactRun:
                    sample = next(
                        sample
                        for sample in case.behavior
                        if sample.arguments == tuple(arguments)
                    )
                    stdout = " ".join(str(value) for value in sample.expected).encode()
                    return _receipt(artifact, image, tuple(arguments), _result(stdout))

                with mock.patch.object(
                    behavior.artifact_run, "run_artifact", side_effect=matching
                ) as run:
                    result = behavior.grade(case, self.artifact, IMAGE, self.limits)

                self.assertEqual(result["status"], "passed")
                self.assertEqual(result["checked"], len(case.behavior))
                self.assertEqual(result["total"], len(case.behavior))
                self.assertEqual(result["artifact_sha256"], self.artifact.sha256)
                self.assertEqual(len(result["observations"]), len(case.behavior))
                self.assertEqual(run.call_count, len(case.behavior))
                for call, sample in zip(run.call_args_list, case.behavior):
                    self.assertIs(call.args[0], self.artifact)
                    self.assertEqual(call.args[1], IMAGE)
                    self.assertEqual(call.args[2], list(sample.arguments))
                    self.assertIs(call.args[3], self.limits)

    def test_wrong_numeric_value_is_a_completed_mismatch(self) -> None:
        case = semantic_cases.cases()[0]
        wrong_index = 4

        def wrong_once(
            artifact: build.Artifact,
            image: str,
            arguments: list[str],
            limits: sandbox.Limits,
        ) -> artifact_run.ArtifactRun:
            del limits
            sample = case.behavior[wrong_index]
            if tuple(arguments) == sample.arguments:
                values = (sample.expected[0] + 1, *sample.expected[1:])
            else:
                values = self._sample_by_arguments(arguments).expected
            return _receipt(artifact, image, tuple(arguments), _result(_wire(values)))

        with mock.patch.object(behavior.artifact_run, "run_artifact", side_effect=wrong_once):
            result = behavior.grade(case, self.artifact, IMAGE, self.limits)

        self.assertEqual(result["status"], "mismatch")
        self.assertEqual(result["checked"], wrong_index + 1)
        self.assertEqual(result["total"], len(case.behavior))
        self.assertEqual(result["observations"][-1]["observed"], [case.behavior[wrong_index].expected[0] + 1])

    def test_wrong_arity_and_sdk_text_are_mismatches(self) -> None:
        case = semantic_cases.cases()[0]
        first = case.behavior[0]
        cases = (
            ("wrong arity", _wire((*first.expected, 99))),
            ("SDK text", b"@reachable 7\n@always 8 1\n"),
        )
        for name, output in cases:
            with self.subTest(name=name):
                receipt = _receipt(
                    self.artifact,
                    IMAGE,
                    first.arguments,
                    _result(output),
                )
                with mock.patch.object(
                    behavior.artifact_run, "run_artifact", return_value=receipt
                ):
                    result = behavior.grade(case, self.artifact, IMAGE, self.limits)
                self.assertEqual(result["status"], "mismatch")
                self.assertEqual(result["checked"], 1)
                if name == "SDK text":
                    self.assertIsNone(result["observations"][0]["observed"])
                else:
                    self.assertNotEqual(result["observations"][0]["observed"], list(first.expected))

    def test_nonzero_and_timeout_are_execution_errors_not_mismatches(self) -> None:
        case = semantic_cases.cases()[0]
        cases = (
            _result(b"999", exit_code=2, stderr=b"application failed"),
            _result(b"999", exit_code=None, termination="timeout"),
        )
        for command_result in cases:
            with self.subTest(command_result=command_result):
                receipt = _receipt(self.artifact, IMAGE, case.behavior[0].arguments, command_result)
                with mock.patch.object(
                    behavior.artifact_run, "run_artifact", return_value=receipt
                ):
                    result = behavior.grade(case, self.artifact, IMAGE, self.limits)
                self.assertEqual(result["status"], "execution_error")
                self.assertNotEqual(result["status"], "mismatch")
                self.assertEqual(result["checked"], 1)
                self.assertEqual(result["observations"][0]["termination"], command_result.termination)

    def test_wrong_execution_receipts_are_rejected(self) -> None:
        case = semantic_cases.cases()[0]
        sample = case.behavior[0]
        output = _wire(sample.expected)
        mutations = (
            {"artifact_sha256": "0" * 64},
            {"receipt_image": "sha256:" + "b" * 64},
            {"receipt_argv": ("wrong",)},
        )
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                receipt = _receipt(
                    self.artifact,
                    IMAGE,
                    sample.arguments,
                    _result(output),
                    **mutation,
                )
                with mock.patch.object(
                    behavior.artifact_run, "run_artifact", return_value=receipt
                ):
                    with self.assertRaises(ValueError):
                        behavior.grade(case, self.artifact, IMAGE, self.limits)

    def test_empty_private_corpus_is_rejected_before_execution(self) -> None:
        case = semantic_cases.Case("empty", "controller", b"", (), ())
        with mock.patch.object(behavior.artifact_run, "run_artifact") as run:
            with self.assertRaises(ValueError):
                behavior.grade(case, self.artifact, IMAGE, self.limits)
        run.assert_not_called()


def _wire(values: tuple[int, ...]) -> bytes:
    return (" ".join(str(value) for value in values) + "\n").encode()


class IntegerWireParserTests(unittest.TestCase):
    def test_whitespace_and_signed_integers_are_supported(self) -> None:
        self.assertEqual(behavior._integers(b"  +12\t-3\n0  "), (12, -3, 0))

    def test_malformed_wire_values_are_rejected(self) -> None:
        invalid = (
            b"1x",
            b"1.0",
            b"1_0",
            b"1" * 21,
            b"1 " * 17,
            bytearray(b"1"),
            "1",
            None,
            1,
        )
        for output in invalid:
            with self.subTest(output=output):
                self.assertIsNone(behavior._integers(output))  # type: ignore[arg-type]


if __name__ == "__main__":
    unittest.main()
