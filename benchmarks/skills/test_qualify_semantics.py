# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import hashlib
import shutil
import tempfile
import unittest
from contextlib import contextmanager
from dataclasses import replace
from argparse import Namespace
from pathlib import Path
from typing import Iterator
from unittest import mock

try:
    from . import behavior, build, checker_guest, materials, qualify_semantics, sandbox, semantic_cases
except ImportError:  # unittest discovery can load this directory as top-level.
    import behavior  # type: ignore[no-redef]
    import build  # type: ignore[no-redef]
    import checker_guest  # type: ignore[no-redef]
    import materials  # type: ignore[no-redef]
    import qualify_semantics  # type: ignore[no-redef]
    import sandbox  # type: ignore[no-redef]
    import semantic_cases  # type: ignore[no-redef]


IMAGE = "sha256:" + "a" * 64
OTHER_IMAGE = "sha256:" + "b" * 64
_VARIANTS = tuple(qualify_semantics._VARIANTS)
_RECIPE = tuple(qualify_semantics._RECIPE)
_REPOSITORY = Path(qualify_semantics.__file__).resolve().parents[2]
_COMMON_DOCS = {
    "docs/faults.md": _REPOSITORY / "workloads/faults/README.md",
    "docs/fault-agent.md": _REPOSITORY / "workloads/fault-agent/README.md",
    "docs/sdk.md": _REPOSITORY / "consonance/harmony-linux/sdk/README.md",
}
_SKILLS_ROOT = _REPOSITORY / "skills"


def _digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _limits() -> sandbox.Limits:
    return sandbox.Limits(
        wall=40,
        toolcalls=2,
        memorybytes=256 * 1024**2,
        workbytes=32 * 1024**2,
        cpus=1,
        pids=64,
        outputbytes=8 * 1024**2,
    )


class QualificationFixture:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.output = root / "qualification"
        self.paths = {
            "harmony": self._write("harmony", b"trusted harmony"),
            "kernel": self._write("kernel", b"trusted kernel"),
            "base": self._write("base", b"trusted base"),
            "agent": self._write("agent", b"trusted agent"),
            "preparer": self._write("preparer", b"trusted preparer"),
        }

    def _write(self, name: str, data: bytes) -> Path:
        path = self.root / name
        path.write_bytes(data)
        return path

    def args(self) -> Namespace:
        return Namespace(image=IMAGE, output=self.output, **self.paths)


def _variant(artifact: build.Artifact) -> str:
    try:
        _case, variant = artifact.data.decode("ascii").split("|", 1)
    except (UnicodeDecodeError, ValueError) as exc:
        raise AssertionError("fixture artifact has no case/variant identity") from exc
    return variant


class BuildStub:
    def __init__(
        self,
        fixture: QualificationFixture,
        mode: str | None = None,
        cohort_mutation: str | None = None,
    ) -> None:
        self.fixture = fixture
        self.mode = mode
        self.cohort_mutation = cohort_mutation
        self.calls: list[tuple[Path, str, str, tuple[str, ...], tuple[str, ...]]] = []
        self.first_build_saw_all_cohorts = False

    @staticmethod
    def _skill_files() -> set[str]:
        return {
            ".agent/skills/" + path.relative_to(_SKILLS_ROOT).as_posix()
            for name in qualify_semantics._SKILLS
            for path in (_SKILLS_ROOT / name).rglob("*")
            if path.is_file()
        }

    @staticmethod
    def _skill_directories() -> set[str]:
        directories = {"docs", ".agent", ".agent/skills"}
        for name in qualify_semantics._SKILLS:
            source = _SKILLS_ROOT / name
            directories.add(".agent/skills/" + name)
            directories.update(
                ".agent/skills/" + path.relative_to(_SKILLS_ROOT).as_posix()
                for path in source.rglob("*")
                if path.is_dir()
            )
        return directories

    def _assert_initial_cohorts(self) -> None:
        expected_skills = self._skill_files()
        expected_docs = {"main.c", *(_COMMON_DOCS.keys()), "TASK.txt", "SETTINGS.json"}
        selected = semantic_cases.cases()
        for case in selected:
            cohort = self.fixture.output / case.name / "cohort"
            materials.verify_pair(cohort)
            docs = cohort / "docs"
            skills = cohort / "skills"
            actual_docs = {
                path.relative_to(docs).as_posix()
                for path in docs.rglob("*")
                if path.is_file()
            }
            self._require(actual_docs == expected_docs, f"unexpected common cohort files: {actual_docs}")
            self._require((docs / "main.c").read_bytes() == case.source, "original source was changed")
            for name, source in _COMMON_DOCS.items():
                self._require((docs / name).read_bytes() == source.read_bytes(), f"common doc changed: {name}")
            actual_skills = {
                path.relative_to(skills).as_posix()
                for path in skills.rglob("*")
                if path.is_file() and ".agent/skills" in path.relative_to(skills).as_posix()
            }
            self._require(actual_skills == expected_skills, "cohort treatment is not the generic skill tree")
            actual_skill_directories = {
                path.relative_to(skills).as_posix()
                for path in skills.rglob("*")
                if path.is_dir()
            }
            self._require(
                actual_skill_directories == self._skill_directories(),
                "cohort treatment directory topology changed",
            )
            for relative in expected_skills:
                source = _SKILLS_ROOT / relative.removeprefix(".agent/skills/")
                target = skills / relative
                self._require(
                    target.read_bytes() == source.read_bytes(),
                    f"generic skill content changed: {relative}",
                )
                self._require(
                    target.stat().st_mode & 0o111 == source.stat().st_mode & 0o111,
                    f"generic skill executable mode changed: {relative}",
                )
            self._require(
                all("grader" not in name and "solution" not in name for name in actual_skills),
                "private grader material entered the treatment arm",
            )
        self.first_build_saw_all_cohorts = True

    @staticmethod
    def _require(condition: bool, message: str) -> None:
        if not condition:
            raise AssertionError(message)

    def __call__(
        self,
        pair: Path,
        manifest_sha256: str,
        image: str,
        argv: list[str],
        outputs: list[str],
        _limits: sandbox.Limits,
        _max_artifact_bytes: int,
    ) -> build.BuildResult:
        if not self.calls:
            self._assert_initial_cohorts()
            self._mutate_cohort_after_first_build_check()
        materials.verify_pair(pair)
        variant = pair.parent.name
        case_name = pair.parent.parent.name
        data = f"{case_name}|{variant}".encode("ascii")
        artifact = build.Artifact("app", data, hashlib.sha256(data).hexdigest(), True)
        self.calls.append((pair, manifest_sha256, image, tuple(argv), tuple(outputs)))
        result_manifest = manifest_sha256
        result_image = image
        result_argv = tuple(argv)
        result_artifacts = (artifact,)
        if self.mode == "manifest":
            result_manifest = "0" * 64
        elif self.mode == "image":
            result_image = OTHER_IMAGE
        elif self.mode == "argv":
            result_argv = ("/wrong/recipe",)
        elif self.mode == "artifacts":
            result_artifacts = (artifact, artifact)
        return build.BuildResult(
            manifest_sha256=result_manifest,
            image=result_image,
            argv=result_argv,
            artifacts=result_artifacts,
            stdout=b"compiler output",
            stderr=b"",
        )

    def _mutate_cohort_after_first_build_check(self) -> None:
        if self.cohort_mutation is None:
            return
        cohort = self.fixture.output / semantic_cases.cases()[0].name / "cohort"
        skill_file = cohort / "skills/.agent/skills/harmony-build/SKILL.md"
        if self.cohort_mutation == "content":
            skill_file.chmod(0o644)
            skill_file.write_bytes(skill_file.read_bytes() + b"\ntampered\n")
            skill_file.chmod(0o444)
        elif self.cohort_mutation == "executable":
            skill_file.chmod(0o555)
        elif self.cohort_mutation == "topology":
            skill_dir = skill_file.parent
            skill_dir.chmod(0o755)
            extra = skill_dir / "unexpected-empty"
            extra.mkdir()
            extra.chmod(0o555)
            skill_dir.chmod(0o555)
        else:
            raise AssertionError(f"unknown cohort mutation: {self.cohort_mutation}")


class BehaviorStub:
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
        variant = _variant(artifact)
        if variant == "changed-application":
            if self.mode == "exception":
                raise RuntimeError("artifact execution infrastructure failed")
            if self.mode == "execution_error":
                status = "execution_error"
            else:
                status = "mismatch"
        else:
            status = "passed"
        return {
            "status": status,
            "checked": len(case.behavior),
            "total": len(case.behavior),
            "artifact_sha256": artifact.sha256,
            "observations": [],
        }


class CheckerStub:
    def __init__(self, mode: dict[str, str] | None = None) -> None:
        self.mode = mode or {}
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
            raise AssertionError("checker received an incomplete trusted-input set")
        if set(pins) != set(paths):
            raise AssertionError("checker received an incomplete pin set")
        if not destination.is_dir():
            raise AssertionError("checker destination was not created")
        variant = _variant(artifact)
        self.calls.append((artifact, arguments, expected_holds, destination))
        action = self.mode.get(variant)
        if action == "error":
            raise RuntimeError("malformed guest checker response")
        if action == "wrong":
            status = "passed" if variant == "always-pass" else "mismatch"
        elif variant == "valid":
            status = "passed"
        elif variant == "always-pass":
            status = "passed" if expected_holds else "mismatch"
        elif variant == "always-fail":
            status = "mismatch" if expected_holds else "passed"
        elif variant == "silent":
            status = "no_telemetry"
        else:
            raise AssertionError(f"changed application reached checker: {variant}")
        return {"status": status, "observed_holds": expected_holds}


class QualifySemanticsTests(unittest.TestCase):
    @contextmanager
    def qualification(
        self,
        *,
        build_mode: str | None = None,
        behavior_mode: str | None = None,
        checker_mode: dict[str, str] | None = None,
        cohort_mutation: str | None = None,
    ) -> Iterator[tuple[dict, QualificationFixture, BuildStub, BehaviorStub, CheckerStub]]:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QualificationFixture(Path(temporary).resolve())
            built = BuildStub(fixture, build_mode, cohort_mutation)
            behavioral = BehaviorStub(behavior_mode)
            checker = CheckerStub(checker_mode)
            with (
                mock.patch.object(
                    qualify_semantics.guest_limits,
                    "verify_output_mount",
                    return_value={"kind": "mock-verified", "path": str(fixture.root)},
                ),
                mock.patch.object(
                    qualify_semantics.tempfile,
                    "gettempdir",
                    return_value=str(fixture.root),
                ),
                mock.patch.object(qualify_semantics.build, "build_frozen", side_effect=built),
                mock.patch.object(qualify_semantics.behavior, "grade", side_effect=behavioral),
                mock.patch.object(
                    qualify_semantics.checker_guest,
                    "check_artifact",
                    side_effect=checker,
                ),
            ):
                result = qualify_semantics.qualify(fixture.args())
                yield result, fixture, built, behavioral, checker

    def test_all_ten_submissions_require_exact_behavior_and_checker_matrix(self) -> None:
        with self.qualification() as (result, fixture, built, behavioral, checker):
            self.assertTrue(result["qualified"])
            self.assertTrue(built.first_build_saw_all_cohorts)
            self.assertEqual(len(built.calls), 10)
            self.assertEqual(len(behavioral.calls), 10)
            self.assertEqual(len(checker.calls), 2 * 4 * 6)
            for case in semantic_cases.cases():
                report = result["cases"][case.name]
                self.assertEqual(report["split"], case.split)
                self.assertEqual(set(report["submissions"]), set(_VARIANTS))
                for variant in _VARIANTS:
                    submission = report["submissions"][variant]
                    self.assertTrue(submission["passed"], (case.name, variant, submission))
                    expected_behavior = "mismatch" if variant == "changed-application" else "passed"
                    self.assertEqual(submission["behavior"]["status"], expected_behavior)
                    controls = submission.get("checker_controls", [])
                    if variant == "changed-application":
                        self.assertEqual(controls, [])
                        continue
                    self.assertEqual(len(controls), len(case.controls))
                    statuses = [item["status"] for item in controls]
                    if variant == "valid":
                        expected = ["passed"] * len(case.controls)
                    elif variant == "always-pass":
                        expected = ["passed" if control.holds else "mismatch" for control in case.controls]
                    elif variant == "always-fail":
                        expected = ["mismatch" if control.holds else "passed" for control in case.controls]
                    else:
                        expected = ["no_telemetry"] * len(case.controls)
                    self.assertEqual(statuses, expected)

    def test_all_cohort_material_is_frozen_before_the_first_build(self) -> None:
        with self.qualification() as (result, fixture, built, _behavioral, _checker):
            self.assertTrue(result["qualified"])
            self.assertTrue(built.first_build_saw_all_cohorts)
            for case in semantic_cases.cases():
                cohort = fixture.output / case.name / "cohort"
                manifest = materials.verify_pair(cohort)
                self.assertEqual(
                    {item["path"] for item in manifest["common"]},
                    {"main.c", *(_COMMON_DOCS.keys())},
                )
                self.assertEqual(
                    {item["path"] for item in manifest["treatment"]},
                    BuildStub._skill_files(),
                )

    def test_compiled_receipt_mismatches_reject_before_behavior(self) -> None:
        for mode in ("manifest", "image", "argv", "artifacts"):
            with self.subTest(mode=mode), self.qualification(build_mode=mode) as (
                result,
                _fixture,
                built,
                behavioral,
                _checker,
            ):
                self.assertFalse(result["qualified"])
                self.assertEqual(len(built.calls), 10)
                self.assertEqual(behavioral.calls, [])
                self.assertTrue(
                    all(
                        not submission["passed"]
                        for case in result["cases"].values()
                        for submission in case["submissions"].values()
                    )
                )

    def test_changed_application_execution_errors_and_exceptions_cannot_count_as_mismatch(self) -> None:
        for mode in ("execution_error", "exception"):
            with self.subTest(mode=mode), self.qualification(behavior_mode=mode) as (
                result,
                _fixture,
                _built,
                behavioral,
                checker,
                ):
                self.assertFalse(result["qualified"])
                self.assertEqual(len(behavioral.calls), 10)
                self.assertEqual(
                    [_variant(artifact) for artifact, *_rest in checker.calls].count(
                        "changed-application"
                    ),
                    0,
                )
                self.assertEqual(len(checker.calls), 2 * 4 * 6)
                for case in semantic_cases.cases():
                    submission = result["cases"][case.name]["submissions"]["changed-application"]
                    self.assertFalse(submission["passed"])

    def test_malformed_guest_error_cannot_count_silent_control(self) -> None:
        with self.qualification(checker_mode={"silent": "error"}) as (
            result,
            _fixture,
            _built,
            _behavioral,
            checker,
        ):
            self.assertFalse(result["qualified"])
            # Each malformed silent guest aborts on its first control; every
            # other non-changed submission still runs all six controls.
            self.assertEqual(len(checker.calls), 2 * (3 * 6 + 1))
            for case in semantic_cases.cases():
                submission = result["cases"][case.name]["submissions"]["silent"]
                self.assertFalse(submission["passed"])
                self.assertIn("malformed guest checker response", submission["error"])

    def test_unexpected_always_pass_and_fail_checker_statuses_fail_the_matrix(self) -> None:
        for variant in ("always-pass", "always-fail"):
            with self.subTest(variant=variant), self.qualification(checker_mode={variant: "wrong"}) as (
                result,
                _fixture,
                built,
                _behavioral,
                _checker,
            ):
                self.assertFalse(result["qualified"])
                self.assertTrue(built.first_build_saw_all_cohorts)
                self.assertEqual(len(built.calls), 10)
                for case in semantic_cases.cases():
                    submission = result["cases"][case.name]["submissions"][variant]
                    self.assertFalse(submission["passed"])

    def assert_selection_rejected(self, selected: tuple[semantic_cases.Case, ...]) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QualificationFixture(Path(temporary).resolve())
            with (
                mock.patch.object(
                    qualify_semantics.guest_limits,
                    "verify_output_mount",
                    return_value={"kind": "mock-verified"},
                ),
                mock.patch.object(qualify_semantics.tempfile, "gettempdir", return_value=str(fixture.root)),
                mock.patch.object(qualify_semantics.semantic_cases, "cases", return_value=selected),
                mock.patch.object(qualify_semantics.build, "build_frozen") as build_call,
                mock.patch.object(qualify_semantics.behavior, "grade") as behavior_call,
            ):
                with self.assertRaises(ValueError):
                    qualify_semantics.qualify(fixture.args())
            build_call.assert_not_called()
            behavior_call.assert_not_called()

    def test_empty_split_and_checker_contracts_are_rejected(self) -> None:
        cases = semantic_cases.cases()
        all_true = replace(
            cases[0],
            controls=tuple(replace(control, holds=True) for control in cases[0].controls),
        )
        missing_split = replace(cases[1], split=cases[0].split)
        empty_behavior = replace(cases[0], behavior=())
        for label, selected in (
            ("empty corpus", ()),
            ("missing split", (cases[0], missing_split)),
            ("missing valid-invalid controls", (all_true, cases[1])),
            ("empty behavior", (empty_behavior, cases[1])),
        ):
            with self.subTest(label=label):
                self.assert_selection_rejected(tuple(selected))

    def test_fixed_corpus_domains_reject_truncated_or_cross_bound_material(self) -> None:
        cases = semantic_cases.cases()
        short_behavior = replace(cases[0], behavior=cases[0].behavior[:-1])
        short_controls = replace(cases[0], controls=cases[0].controls[:-1])
        changed_source = replace(cases[1], source=cases[0].source)
        for label, selected in (
            ("truncated development behavior", (short_behavior, cases[1])),
            ("truncated checker controls", (short_controls, cases[1])),
            ("duplicate source across domains", (cases[0], changed_source)),
        ):
            with self.subTest(label=label):
                self.assert_selection_rejected(tuple(selected))

    def test_treatment_content_mode_and_topology_mutations_are_rejected(self) -> None:
        for mutation, message in (
            ("content", "file hash mismatch"),
            ("executable", "file hash mismatch"),
            ("topology", "missing or extra directories"),
        ):
            with self.subTest(mutation=mutation):
                with self.assertRaisesRegex(materials.MaterialError, message):
                    with self.qualification(cohort_mutation=mutation):
                        pass

    def test_verified_modified_treatment_pair_fails_baseline_before_build(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = QualificationFixture(Path(temporary).resolve())
            built = BuildStub(fixture)
            behavioral = BehaviorStub()
            copied_skills = fixture.root / "copied-skills"
            shutil.copytree(_SKILLS_ROOT, copied_skills)
            modified_skill = copied_skills / "harmony-build/SKILL.md"
            modified_skill.write_bytes(modified_skill.read_bytes() + b"\nmodified\n")
            modified_skill.chmod((modified_skill.stat().st_mode & 0o777) | 0o111)
            empty_directory = copied_skills / "harmony-build/empty-directory"
            empty_directory.mkdir()

            real_freeze_pair = materials.freeze_pair

            def freeze_pair_with_modified_treatment(
                destination: Path,
                common: dict[str, Path],
                skills: dict[str, Path],
                prompt: str,
                settings: dict,
            ) -> dict:
                if destination.name == "cohort":
                    skills = {
                        name: copied_skills / name
                        for name in qualify_semantics._SKILLS
                    }
                manifest = real_freeze_pair(destination, common, skills, prompt, settings)
                # Keep this fixture structurally valid so the rejection below
                # is the pinned-baseline check, rather than arm corruption.
                if destination.name == "cohort":
                    assert manifest == materials.verify_pair(destination)
                return manifest

            with (
                mock.patch.object(
                    qualify_semantics.guest_limits,
                    "verify_output_mount",
                    return_value={"kind": "mock-verified", "path": str(fixture.root)},
                ),
                mock.patch.object(
                    qualify_semantics.tempfile,
                    "gettempdir",
                    return_value=str(fixture.root),
                ),
                mock.patch.object(
                    qualify_semantics.materials,
                    "freeze_pair",
                    side_effect=freeze_pair_with_modified_treatment,
                ),
                mock.patch.object(qualify_semantics.build, "build_frozen", side_effect=built),
                mock.patch.object(qualify_semantics.behavior, "grade", side_effect=behavioral),
            ):
                with self.assertRaisesRegex(ValueError, "generic skills differ from the frozen baseline"):
                    qualify_semantics.qualify(fixture.args())

            self.assertEqual(built.calls, [])
            self.assertEqual(behavioral.calls, [])

    def test_tmpdir_guard_uses_real_existing_device_and_rejects_outside_path(self) -> None:
        with self.qualification() as (result, fixture, _built, _behavioral, _checker):
            self.assertTrue(result["qualified"])
            self.assertTrue(Path(result["host_tmpdir"]).exists())
            self.assertEqual(
                Path(result["host_tmpdir"]).stat().st_dev,
                fixture.output.parent.stat().st_dev,
            )

        with tempfile.TemporaryDirectory() as temporary, tempfile.TemporaryDirectory() as outside:
            fixture = QualificationFixture(Path(temporary).resolve())
            with (
                mock.patch.object(
                    qualify_semantics.guest_limits,
                    "verify_output_mount",
                    return_value={"kind": "mock-verified"},
                ),
                mock.patch.object(qualify_semantics.tempfile, "gettempdir", return_value=outside),
            ):
                with self.assertRaisesRegex(ValueError, "host TMPDIR"):
                    qualify_semantics.qualify(fixture.args())
            self.assertFalse(fixture.output.exists())


if __name__ == "__main__":
    unittest.main()
