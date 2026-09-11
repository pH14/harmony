# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import hashlib
import json
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

try:
    from . import build, materials, qualify_guest
    from .test_guest_evidence import fixture
except ImportError:  # unittest discovery can load this directory as top-level.
    import build  # type: ignore[no-redef]
    import materials  # type: ignore[no-redef]
    import qualify_guest  # type: ignore[no-redef]
    from test_guest_evidence import fixture  # type: ignore[no-redef]


IMAGE = "sha256:" + "44" * 32


def _digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _process_result(
    *,
    stdout: bytes = b"",
    stderr: bytes = b"",
    exit_code: int = 0,
) -> SimpleNamespace:
    return SimpleNamespace(
        exit_code=exit_code,
        stdout=stdout,
        stderr=stderr,
        timed_out=False,
        output_overflow=False,
    )


class QualificationFixture:
    def __init__(
        self,
        root: Path,
        *,
        preparer_mode: str = "valid",
        mutate_input: str | None = None,
        symlink_report: bool = False,
    ) -> None:
        self.root = root
        self.preparer_mode = preparer_mode
        self.mutate_input = mutate_input
        self.symlink_report = symlink_report
        self.preparer_calls: list[list[str]] = []
        self.cli_calls: list[list[str]] = []
        self.build_calls: list[tuple[Path, str, str, list[str]]] = []

        self.harmony = self._write("harmony", b"pinned-cli")
        self.kernel = self._write("kernel", b"pinned-kernel")
        self.base = self._write("base", b"pinned-base")
        self.agent = self._write("agent", b"pinned-agent")
        self.preparer = self._write("preparer", b"pinned-preparer")

    def _write(self, name: str, data: bytes) -> Path:
        path = self.root / name
        path.write_bytes(data)
        return path

    def args(self) -> Namespace:
        return Namespace(
            image=IMAGE,
            harmony=self.harmony,
            kernel=self.kernel,
            base=self.base,
            agent=self.agent,
            preparer=self.preparer,
            output=self.root / "qualification",
        )

    def build_frozen(
        self,
        pair: Path,
        manifest: str,
        image: str,
        argv: list[str],
        _outputs: list[str],
        _limits: object,
        _max_artifact_bytes: int,
    ) -> build.BuildResult:
        self.build_calls.append((pair, manifest, image, list(argv)))
        data = b"\x7fELF" + len(self.build_calls).to_bytes(2, "little") + b"fixture"
        artifact = build.Artifact("app", data, _digest(data), True)
        return build.BuildResult(
            manifest_sha256=manifest,
            image=image,
            argv=tuple(argv),
            artifacts=(artifact,),
            stdout=b"compiler output",
            stderr=b"",
        )

    def _write_guest_evidence(self, argv: list[str]) -> None:
        out = Path(argv[argv.index("--out") + 1])
        image = Path(argv[argv.index("--package") + 2])
        actions = Path(argv[argv.index("--replay") + 1])
        out.mkdir()

        violation = out.parent.name == "violation"
        report, sidecar = fixture(violation=violation)
        report["image_sha256"] = _digest(b"prepared" + image.read_bytes())
        report["kernel_sha256"] = _digest(self.kernel.read_bytes())
        report["fault_agent_sha256"] = _digest(self.agent.read_bytes())
        if out.parent.name == "silent":
            report["replays"][0]["sometimes"] = []
            sidecar["events"] = []

        report_bytes = json.dumps(report, sort_keys=True).encode("utf-8")
        sidecar_bytes = json.dumps(sidecar, sort_keys=True).encode("utf-8")
        report_path = out / "report.json"
        if self.symlink_report and out.parent.name == "positive":
            target = out.parent / "report-outside.json"
            target.write_bytes(report_bytes)
            report_path.symlink_to(target)
        else:
            report_path.write_bytes(report_bytes)
        (out / "replay-1-events.json").write_bytes(sidecar_bytes)

        if self.mutate_input == "archive" and out.parent.name == "positive":
            image.chmod(0o644)
            image.write_bytes(image.read_bytes() + b"archive mutation")
        if self.mutate_input == "actions" and out.parent.name == "positive":
            actions.chmod(0o644)
            actions.write_bytes(actions.read_bytes() + b"action mutation")

    def run_bounded(self, argv: list[str], **_kwargs: object) -> SimpleNamespace:
        if argv[0] == str(self.preparer):
            self.preparer_calls.append(list(argv))
            image_digest = _digest(b"prepared" + Path(argv[1]).read_bytes())
            if self.preparer_mode == "wrong":
                image_digest = _digest(b"wrong prepared image")
            if self.preparer_mode == "malformed":
                return _process_result(stdout=b"not-a-digest\n")
            if self.preparer_mode == "nonzero":
                return _process_result(stdout=(b"0" * 64) + b"\n", exit_code=7)
            return _process_result(stdout=image_digest.encode("ascii") + b"\n")

        self.cli_calls.append(list(argv))
        self._write_guest_evidence(argv)
        return _process_result(stdout=b"guest output\n")


class QualifyGuestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def qualify(self, fixture_case: QualificationFixture) -> dict:
        with (
            patch.object(qualify_guest.build, "build_frozen", side_effect=fixture_case.build_frozen),
            patch.object(qualify_guest.sandbox, "_run_bounded", side_effect=fixture_case.run_bounded),
            patch.object(
                qualify_guest.guest_limits,
                "verify_output_mount",
                return_value={"kind": "mock-immutable", "path": str(self.root)},
            ),
        ):
            return qualify_guest.qualify(fixture_case.args())

    def test_three_cases_use_real_freeze_package_reads_and_digest_pins(self) -> None:
        fixture_case = QualificationFixture(self.root)
        result = self.qualify(fixture_case)

        self.assertTrue(result["qualified"])
        self.assertEqual(set(result["checks"]), {"positive", "violation", "silent"})
        self.assertEqual(len(fixture_case.preparer_calls), 3)
        self.assertEqual(len(fixture_case.cli_calls), 3)
        self.assertEqual(result["output_mount"]["kind"], "mock-immutable")
        for label in ("positive", "violation", "silent"):
            case = self.root / "qualification" / label
            materials.verify_pair(case / "source")
            archive = case / "fixture.tar"
            actions = case / "actions.json"
            archive_digest = _digest(archive.read_bytes())
            prepared_digest = _digest(b"prepared" + archive.read_bytes())
            self.assertEqual(result["checks"][label]["image_archive_sha256"], archive_digest)
            self.assertEqual(result["checks"][label]["actions_sha256"], _digest(actions.read_bytes()))
            self.assertEqual(
                result["checks"][label]["prepared_image_sha256"],
                prepared_digest,
            )
            self.assertNotEqual(prepared_digest, archive_digest)
        self.assertEqual(
            result["input_sha256"]["kernel"],
            _digest(self.root.joinpath("kernel").read_bytes()),
        )

    def test_wrong_prepared_digest_rejects_positive_violation_and_silent(self) -> None:
        fixture_case = QualificationFixture(self.root, preparer_mode="wrong")
        result = self.qualify(fixture_case)

        self.assertFalse(result["qualified"])
        self.assertEqual(len(fixture_case.cli_calls), 3)
        for check in result["checks"].values():
            self.assertFalse(check["passed"])
            self.assertIn("prepared supplied archive", check["detail"])

    def test_cli_input_mutation_is_rejected_after_paths_are_made_writable(self) -> None:
        for input_name in ("archive", "actions"):
            with self.subTest(input_name=input_name):
                case_root = self.root / input_name
                case_root.mkdir()
                fixture_case = QualificationFixture(case_root, mutate_input=input_name)
                result = self.qualify(fixture_case)

                self.assertFalse(result["qualified"])
                self.assertFalse(result["checks"]["positive"]["passed"])
                self.assertIn("pinned execution inputs changed", result["checks"]["positive"]["detail"])
                self.assertEqual(result["checks"]["violation"]["passed"], True)
                self.assertEqual(result["checks"]["silent"]["passed"], True)

    def test_malformed_or_nonzero_preparer_stops_before_cli(self) -> None:
        for mode in ("malformed", "nonzero"):
            with self.subTest(mode=mode):
                case_root = self.root / mode
                case_root.mkdir()
                fixture_case = QualificationFixture(case_root, preparer_mode=mode)
                result = self.qualify(fixture_case)
                self.assertFalse(result["qualified"])
                self.assertEqual(len(fixture_case.preparer_calls), 3)
                self.assertEqual(fixture_case.cli_calls, [])
                self.assertTrue(all(not check["passed"] for check in result["checks"].values()))

    def test_report_symlink_is_rejected_by_anchored_reader(self) -> None:
        fixture_case = QualificationFixture(self.root, symlink_report=True)
        result = self.qualify(fixture_case)

        self.assertFalse(result["qualified"])
        self.assertFalse(result["checks"]["positive"]["passed"])
        self.assertIn("following links", result["checks"]["positive"]["detail"])
        self.assertTrue(result["checks"]["violation"]["passed"])
        self.assertTrue(result["checks"]["silent"]["passed"])


if __name__ == "__main__":
    unittest.main()
