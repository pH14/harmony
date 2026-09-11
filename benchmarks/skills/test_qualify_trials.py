# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import hashlib
import json
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator
from unittest import mock

try:
    from . import build, materials, qualify_semantics, sandbox, semantic_cases, trial
    from .test_qualify_semantics import (
        IMAGE,
        _COMMON_DOCS,
        _SKILLS_ROOT,
        _VARIANTS,
        QualificationFixture,
    )
except ImportError:  # unittest discovery can load this directory as top-level.
    import build  # type: ignore[no-redef]
    import materials  # type: ignore[no-redef]
    import qualify_semantics  # type: ignore[no-redef]
    import sandbox  # type: ignore[no-redef]
    import semantic_cases  # type: ignore[no-redef]
    import trial  # type: ignore[no-redef]
    from test_qualify_semantics import (  # type: ignore[no-redef]
        IMAGE,
        _COMMON_DOCS,
        _SKILLS_ROOT,
        _VARIANTS,
        QualificationFixture,
    )


TRIAL_MAX_SOURCE = 64 * 1024
TRIAL_OUTPUTS = ["main.c"]
TRIAL_LIMITS = sandbox.Limits(
    wall=30,
    toolcalls=3,
    memorybytes=128 * 1024**2,
    workbytes=16 * 1024**2,
    cpus=1,
    pids=64,
    outputbytes=512 * 1024,
)


def _digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _run_result(
    *,
    stdout: bytes = b"",
    stderr: bytes = b"",
    exit_code: int | None = 0,
    termination: str = "exited",
) -> sandbox.RunResult:
    return sandbox.RunResult(exit_code, stdout, stderr, termination)


def _trial_settings(pair: Path, arm: str = "docs") -> dict[str, object]:
    settings = json.loads((pair / arm / "SETTINGS.json").read_text(encoding="utf-8"))
    if type(settings) is not dict or type(settings.get("trial")) is not dict:
        raise AssertionError("fixture has no trial settings")
    return settings["trial"]


def _artifact_identity(artifact: build.Artifact) -> tuple[str, str]:
    fields = artifact.data.decode("ascii").split("|")
    if len(fields) < 2:
        raise AssertionError("trial artifact has no case/variant identity")
    return fields[0], fields[1]


class TrialSessionStub:
    """A bounded fake for TrialSession, retaining the real pair as its input."""

    def __init__(
        self,
        owner: "TrialSessionFactory",
        pair: Path,
        manifest_sha256: str,
        arm: str,
        image: str,
        limits: sandbox.Limits,
        outputs: list[str],
        maximum: int,
    ) -> None:
        self.owner = owner
        self.pair = pair
        self.manifest_sha256 = manifest_sha256
        self.arm = arm
        self.image = image
        self.limits = limits
        self.outputs = tuple(outputs)
        self.maximum = maximum
        self.commands: list[tuple[str, ...]] = []
        self.results: list[sandbox.RunResult] = []
        self.workspace: dict[str, bytes] = {
            str(path.relative_to(pair / arm)): path.read_bytes()
            for path in sorted((pair / arm).rglob("*"))
            if path.is_file()
        }
        self.closed = False
        self.entered = False
        self.finished = False
        self._candidate: bytes | None = None

        self.owner._before_first_capture()
        materials.verify_pair(pair)
        if _digest(pair / "manifest.json") != manifest_sha256:
            raise AssertionError("trial session received a wrong cohort pin")
        if arm not in ("docs", "skills"):
            raise AssertionError("trial session received an unknown arm")
        expected = {
            "image": image,
            "limits": vars(limits).copy(),
            "outputs": list(outputs),
            "max_source_bytes": maximum,
        }
        if _trial_settings(pair, arm) != expected:
            raise AssertionError("trial settings do not match the requested session")
        if _trial_settings(pair, "docs") != _trial_settings(pair, "skills"):
            raise AssertionError("paired arms do not have the same trial settings")
        self.owner.sessions.append(self)

    def __enter__(self) -> "TrialSessionStub":
        if self.closed:
            raise sandbox.SandboxError("trial session is closed")
        self.entered = True
        return self

    def __exit__(self, *_: object) -> bool:
        self.close()
        return False

    def close(self) -> None:
        self.closed = True

    def _record(self, argv: list[str], result: sandbox.RunResult) -> sandbox.RunResult:
        self.commands.append(tuple(argv))
        self.results.append(result)
        return result

    def run(self, argv: list[str]) -> sandbox.RunResult:
        if self.closed or not self.entered:
            raise sandbox.SandboxError("trial session is not active")
        sandbox._validate_argv(argv)
        if self.owner._should_fail(self, "run"):
            raise sandbox.SandboxError("scripted trial capture failed")
        if len(argv) < 5 or argv[0] != "/usr/bin/python3" or argv[1:3] != ["-I", "-S"]:
            raise AssertionError("trial capture used an unexpected command")
        script = argv[4]
        if script == qualify_semantics._INVENTORY:
            expected = {
                path: hashlib.sha256(data).hexdigest()
                for path, data in self.workspace.items()
            }
            if self.owner.mode == "inventory_extra" and self.arm == self.owner.target_arm:
                expected["private-not-requested"] = hashlib.sha256(b"private").hexdigest()
            payload = {"frozen": expected, "workspace": expected}
            return self._record(argv, _run_result(stdout=json.dumps(payload, sort_keys=True).encode()))
        if script != qualify_semantics._WRITE_SOURCE or len(argv) != 6:
            raise AssertionError("trial capture did not use the controller write script")
        try:
            candidate = bytes.fromhex(argv[5])
        except ValueError as error:
            raise AssertionError("trial capture candidate was not hexadecimal") from error
        if self.owner._should_fail(self, "write"):
            raise sandbox.SandboxError("scripted source edit failed")
        self._candidate = candidate
        self.workspace["main.c"] = candidate
        return self._record(argv, _run_result())

    def finish(self) -> trial.Submission:
        if self.closed or not self.entered:
            raise sandbox.SandboxError("trial session is not active")
        self.finished = True
        self.closed = True
        if self.owner._should_fail(self, "finish"):
            raise sandbox.SandboxError("scripted trial cleanup failed")
        if self._candidate is None:
            raise AssertionError("trial finished before its source edit")
        candidate = self._candidate
        if self.owner.mode == "captured_bytes" and self.arm == self.owner.target_arm:
            candidate = b"captured bytes differ"
        source = build.Artifact("main.c", candidate, hashlib.sha256(candidate).hexdigest(), False)
        if self.owner.mode == "receipt" and self.arm == self.owner.target_arm:
            manifest = "0" * 64
        else:
            manifest = self.manifest_sha256
        self.owner.completed.append(self)
        return trial.Submission(
            manifest_sha256=manifest,
            arm=self.arm,  # type: ignore[arg-type]
            image=self.image,
            sources=(source,),
            agent_toolcalls=len(self.commands),
            commands=() if self.owner.mode == "commands" else tuple(self.commands),
            results=() if self.owner.mode == "results" else tuple(self.results),
        )


class TrialSessionFactory:
    def __init__(self, fixture: QualificationFixture, mode: str | None = None) -> None:
        self.fixture = fixture
        self.mode = mode
        self.target_arm = "docs"
        self.sessions: list[TrialSessionStub] = []
        self.completed: list[TrialSessionStub] = []
        self._checked_initial = False

    def __call__(
        self,
        pair: Path,
        manifest_sha256: str,
        arm: str,
        image: str,
        limits: sandbox.Limits,
        outputs: list[str],
        maximum: int,
    ) -> TrialSessionStub:
        # The production helper imports TrialSession and calls it positionally.
        return TrialSessionStub(
            self,
            pair,
            manifest_sha256,
            arm,
            image,
            limits,
            outputs,
            maximum,
        )

    def _before_first_capture(self) -> None:
        if self._checked_initial:
            return
        self._checked_initial = True
        if not self.fixture.output.is_dir():
            raise AssertionError("trial capture started before output setup")
        for case in semantic_cases.cases():
            cohort = self.fixture.output / case.name / "cohort"
            materials.verify_pair(cohort)
            for arm in ("docs", "skills"):
                arm_root = cohort / arm
                if (arm_root / "main.c").read_bytes() != case.source:
                    raise AssertionError("trial capture started after the original source changed")
                if any(arm_root.parent.rglob("submission")):
                    raise AssertionError("a submission was built before the first trial capture")
        if self.sessions:
            raise AssertionError("initial cohort check ran after a capture")

    def _should_fail(self, session: TrialSessionStub, phase: str) -> bool:
        if session.arm != self.target_arm:
            return False
        if self.mode == "capture_failure" and phase == "run":
            return True
        if self.mode == "cleanup_failure" and phase == "finish":
            return True
        return False


class TrialBuildStub:
    def __init__(self, fixture: QualificationFixture, factory: TrialSessionFactory, mode: str | None = None) -> None:
        self.fixture = fixture
        self.factory = factory
        self.mode = mode
        self.calls: list[tuple[Path, str, str, tuple[str, ...], tuple[str, ...]]] = []
        self.first_build_saw_all_cohorts = False

    def _assert_initial_cohorts(self) -> None:
        for case in semantic_cases.cases():
            cohort = self.fixture.output / case.name / "cohort"
            materials.verify_pair(cohort)
            for arm in ("docs", "skills"):
                if (cohort / arm / "main.c").read_bytes() != case.source:
                    raise AssertionError("original cohort source changed before builds")
            docs_files = {
                path.relative_to(cohort / "docs").as_posix()
                for path in (cohort / "docs").rglob("*")
                if path.is_file()
            }
            if docs_files != {"main.c", *_COMMON_DOCS, "TASK.txt", "SETTINGS.json"}:
                raise AssertionError("docs arm does not contain only expected common material")
            skill_files = {
                path.relative_to(cohort / "skills").as_posix()
                for path in (cohort / "skills").rglob("*")
                if path.is_file() and ".agent/skills" in path.relative_to(cohort / "skills").as_posix()
            }
            expected_skills = {
                ".agent/skills/" + path.relative_to(_SKILLS_ROOT).as_posix()
                for name in qualify_semantics._SKILLS
                for path in (_SKILLS_ROOT / name).rglob("*")
                if path.is_file()
            }
            if skill_files != expected_skills:
                raise AssertionError("skills arm does not contain the generic skills")
        self.first_build_saw_all_cohorts = True

    def __call__(
        self,
        pair: Path,
        manifest_sha256: str,
        image: str,
        argv: list[str],
        outputs: list[str],
        limits: sandbox.Limits,
        max_artifact_bytes: int,
    ) -> build.BuildResult:
        if not self.calls:
            self._assert_initial_cohorts()
        if not self.factory.sessions or not all(session.closed for session in self.factory.sessions):
            raise AssertionError("build started before the trial session closed")
        materials.verify_pair(pair)
        if _digest(pair / "manifest.json") != manifest_sha256:
            raise AssertionError("build received a wrong submission pin")
        case_name = pair.parent.parent.parent.name
        variant = pair.parent.name
        case = next(case for case in semantic_cases.cases() if case.name == case_name)
        expected_source = semantic_cases.qualification_source(case, variant)
        if (pair / "docs" / "main.c").read_bytes() != expected_source:
            raise AssertionError("build did not receive the captured source bytes")
        self.calls.append((pair, manifest_sha256, image, tuple(argv), tuple(outputs)))
        arm = pair.parent.parent.name
        if self.mode == "artifact" and arm == "skills":
            data = f"{case_name}|{variant}|skills".encode("ascii")
        else:
            data = f"{case_name}|{variant}".encode("ascii")
        artifact = build.Artifact("app", data, hashlib.sha256(data).hexdigest(), True)
        return build.BuildResult(
            manifest_sha256=manifest_sha256,
            image=image,
            argv=tuple(argv),
            artifacts=(artifact,),
            stdout=b"compiler output",
            stderr=b"",
        )


class TrialBehaviorStub:
    def __init__(self, mode: str | None = None) -> None:
        self.mode = mode
        self.calls: list[tuple[semantic_cases.Case, build.Artifact, str, sandbox.Limits]] = []

    def __call__(
        self,
        case: semantic_cases.Case,
        artifact: build.Artifact,
        image: str,
        limits: sandbox.Limits,
        *,
        run: object = None,
    ) -> dict[str, object]:
        if not callable(run):
            raise AssertionError("qualifier did not pass its bounded artifact dispatcher")
        self.calls.append((case, artifact, image, limits))
        case_name, variant = _artifact_identity(artifact)
        if case_name != case.name:
            raise AssertionError("behavior received an artifact for another case")
        result: dict[str, object] = {
            "status": "mismatch" if variant == "changed-application" else "passed",
            "checked": len(case.behavior),
            "total": len(case.behavior),
            "artifact_sha256": artifact.sha256,
            "observations": [],
        }
        call_index = len(self.calls) - 1
        # qualify() visits docs then skills for each case, five variants per
        # arm. The public behavior callback receives only the artifact, so the
        # call position is the stable arm identity for this controlled fake.
        arm = "docs" if call_index % (2 * len(_VARIANTS)) < len(_VARIANTS) else "skills"
        if self.mode == "behavior" and arm == "skills":
            result["paired_marker"] = "skills"
        return result


class TrialCheckerStub:
    def __init__(self, mode: str | None = None) -> None:
        self.mode = mode
        self.calls: list[tuple[build.Artifact, tuple[str, ...], bool, Path]] = []

    def __call__(
        self,
        artifact: build.Artifact,
        arguments: tuple[str, ...],
        expected_holds: bool,
        paths: dict[str, Path],
        pins: dict[str, str],
        destination: Path,
    ) -> dict[str, object]:
        if set(paths) != {"harmony", "kernel", "base", "agent", "preparer"}:
            raise AssertionError("checker received incomplete trusted paths")
        if set(pins) != set(paths):
            raise AssertionError("checker received incomplete trusted pins")
        if not destination.is_dir():
            raise AssertionError("checker destination was not created")
        case_name, variant = _artifact_identity(artifact)
        self.calls.append((artifact, arguments, expected_holds, destination))
        if variant == "valid":
            status = "passed"
        elif variant == "always-pass":
            status = "passed" if expected_holds else "mismatch"
        elif variant == "always-fail":
            status = "mismatch" if expected_holds else "passed"
        elif variant == "silent":
            status = "no_telemetry"
        else:
            raise AssertionError("changed application reached checker")
        run = destination / "run"
        run.mkdir()
        sidecar: dict[str, object] = {
            "format": "harmony-test-replay-events-v1",
            "case": case_name,
            "variant": variant,
            "arguments": list(arguments),
            "holds": expected_holds,
        }
        if self.mode == "events" and destination.parent.parent.name == "skills":
            sidecar["paired_marker"] = "skills"
        (run / "replay-1-events.json").write_text(
            json.dumps(sidecar, sort_keys=True), encoding="utf-8"
        )
        return {"status": status, "observed_holds": expected_holds}


class QualifyTrialArmTests(unittest.TestCase):
    @contextmanager
    def qualification(
        self,
        *,
        trial_mode: str | None = None,
        build_mode: str | None = None,
        behavior_mode: str | None = None,
        checker_mode: str | None = None,
    ) -> Iterator[tuple[dict, QualificationFixture, TrialSessionFactory, TrialBuildStub, TrialBehaviorStub, TrialCheckerStub]]:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QualificationFixture(Path(temporary).resolve())
            sessions = TrialSessionFactory(fixture, trial_mode)
            built = TrialBuildStub(fixture, sessions, build_mode)
            behavioral = TrialBehaviorStub(behavior_mode)
            checker = TrialCheckerStub(checker_mode)
            args = fixture.args()
            args.trial_arms = True
            with (
                mock.patch.object(
                    qualify_semantics.guest_limits,
                    "verify_output_mount",
                    return_value={"kind": "mock-verified", "path": str(fixture.root)},
                ),
                mock.patch.object(qualify_semantics.tempfile, "gettempdir", return_value=str(fixture.root)),
                mock.patch.object(qualify_semantics.build, "build_frozen", side_effect=built),
                mock.patch.object(qualify_semantics.behavior, "grade", side_effect=behavioral),
                mock.patch.object(qualify_semantics.checker_guest, "check_artifact", side_effect=checker),
                mock.patch.object(trial, "TrialSession", side_effect=sessions),
            ):
                result = qualify_semantics.qualify(args)
                yield result, fixture, sessions, built, behavioral, checker

    def test_both_arms_are_frozen_first_and_run_identical_complete_trials(self) -> None:
        with self.qualification() as (result, fixture, sessions, built, behavioral, checker):
            self.assertTrue(result["qualified"], result)
            self.assertTrue(result["paired_identity"]["passed"])
            self.assertTrue(sessions._checked_initial)
            self.assertTrue(built.first_build_saw_all_cohorts)
            self.assertEqual(len(sessions.sessions), 20)
            self.assertEqual(len(sessions.completed), 20)
            self.assertEqual(len(built.calls), 20)
            self.assertEqual(len(behavioral.calls), 20)
            self.assertEqual(len(checker.calls), 96)
            self.assertEqual(result["paired_identity"]["submissions"], 10)
            self.assertEqual(result["paired_identity"]["guest_controls"], 48)

            by_arm_and_candidate = {
                (session.pair.parent.name, session.arm, session._candidate): session
                for session in sessions.sessions
            }
            self.assertEqual(len(by_arm_and_candidate), 20)
            for case in semantic_cases.cases():
                docs = [
                    session
                    for session in sessions.sessions
                    if session.pair.parent.name == case.name and session.arm == "docs"
                ]
                skills = [
                    session
                    for session in sessions.sessions
                    if session.pair.parent.name == case.name and session.arm == "skills"
                ]
                self.assertEqual(len(docs), len(_VARIANTS))
                self.assertEqual(len(skills), len(_VARIANTS))
                for left, right in zip(docs, skills):
                    self.assertEqual(left.manifest_sha256, right.manifest_sha256)
                    self.assertEqual(left.commands, right.commands)
                    self.assertEqual(left.outputs, tuple(TRIAL_OUTPUTS))
                    self.assertEqual(left.maximum, TRIAL_MAX_SOURCE)
                    self.assertEqual(vars(left.limits), vars(TRIAL_LIMITS))
                    self.assertTrue(left.closed)
                    self.assertTrue(right.closed)

            for pair, manifest, image, argv, outputs in built.calls:
                self.assertEqual(manifest, _digest(pair / "manifest.json"))
                self.assertEqual(image, IMAGE)
                self.assertEqual(argv, tuple(qualify_semantics._RECIPE))
                self.assertEqual(outputs, ("app",))
                self.assertTrue(all(session.closed for session in sessions.sessions))
                self.assertEqual((pair / "docs" / "main.c").stat().st_mode & 0o111, 0)

            for case in semantic_cases.cases():
                self.assertEqual(
                    result["cases"][case.name]["arms"]["docs"]["submissions"].keys(),
                    result["cases"][case.name]["arms"]["skills"]["submissions"].keys(),
                )

            for case in semantic_cases.cases():
                cohort = fixture.output / case.name / "cohort"
                self.assertEqual(materials.verify_pair(cohort), materials.verify_pair(cohort))
                self.assertEqual(
                    _trial_settings(cohort, "docs"),
                    {
                        "image": IMAGE,
                        "limits": vars(TRIAL_LIMITS),
                        "outputs": TRIAL_OUTPUTS,
                        "max_source_bytes": TRIAL_MAX_SOURCE,
                    },
                )
                self.assertEqual(_trial_settings(cohort, "docs"), _trial_settings(cohort, "skills"))

    def test_capture_and_receipt_failures_never_receive_qualified_credit(self) -> None:
        for mode in ("inventory_extra", "captured_bytes", "receipt", "commands", "results",
                     "capture_failure", "cleanup_failure"):
            with self.subTest(mode=mode), self.qualification(trial_mode=mode) as (
                result,
                _fixture,
                sessions,
                built,
                behavioral,
                _checker,
            ):
                self.assertFalse(result["qualified"])
                self.assertFalse(result["paired_identity"]["passed"])
                self.assertTrue(sessions.sessions)
                self.assertTrue(all(session.closed for session in sessions.sessions))
                self.assertLess(len(built.calls), 20)
                self.assertLess(len(behavioral.calls), 20)

    def test_paired_artifact_behavior_and_complete_event_differences_fail_after_passing_statuses(self) -> None:
        for kind, kwargs in (
            ("artifact", {"build_mode": "artifact"}),
            ("behavior", {"behavior_mode": "behavior"}),
            ("events", {"checker_mode": "events"}),
        ):
            with self.subTest(kind=kind), self.qualification(**kwargs) as (
                result,
                _fixture,
                _sessions,
                built,
                behavioral,
                checker,
            ):
                self.assertFalse(result["qualified"], result)
                self.assertFalse(result["paired_identity"]["passed"])
                self.assertEqual(len(built.calls), 20)
                self.assertEqual(len(behavioral.calls), 20)
                self.assertEqual(len(checker.calls), 96)
                self.assertTrue(
                    all(
                        submission["passed"]
                        for case in result["cases"].values()
                        for arm in case["arms"].values()
                        for submission in arm["submissions"].values()
                    )
                )


if __name__ == "__main__":
    unittest.main()
