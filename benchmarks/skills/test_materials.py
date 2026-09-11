# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import importlib.util
import json
import os
import shutil
import stat
import tempfile
import unittest
from pathlib import Path
from unittest import mock


_MODULE_PATH = Path(__file__).with_name("materials.py")
_SPEC = importlib.util.spec_from_file_location("skill_materials_under_test", _MODULE_PATH)
materials = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(materials)


class MaterialsTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temporary = tempfile.TemporaryDirectory()
        self.root = Path(self._temporary.name).resolve()

    def tearDown(self) -> None:
        self._temporary.cleanup()

    def _file(self, name: str, data: bytes = b"source") -> Path:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def _pair(self, common: dict[str, Path] | None = None, skills: dict[str, Path] | None = None) -> tuple[Path, dict]:
        common_source = self._file("common/input.txt", b"common bytes")
        treatment_source = self._file("treatment/input.txt", b"treatment bytes")
        destination = self.root / f"pair-{len(list(self.root.glob('pair-*')))}"
        manifest = materials.freeze_pair(
            destination,
            {"input.txt": common_source} if common is None else common,
            {"input.txt": treatment_source} if skills is None else skills,
            "a deterministic task",
            {"z": [2, 3], "a": 1},
        )
        return destination, manifest

    def test_freeze_copies_independent_common_and_treatment_arms(self) -> None:
        common_file = self._file("common/guide.txt", b"guide")
        common_tree = self.root / "common/tree"
        common_tree.mkdir(parents=True)
        (common_tree / "nested.txt").write_bytes(b"nested")
        treatment_tree = self.root / "treatment/rules"
        treatment_tree.mkdir(parents=True)
        (treatment_tree / "rule.txt").write_bytes(b"rule")

        destination = self.root / "pair"
        manifest = materials.freeze_pair(
            destination,
            {"guide.txt": common_file, "tree": common_tree},
            {"rules": treatment_tree},
            "prompt text",
            {"z": [2, 3], "a": 1},
        )

        self.assertEqual(manifest, materials.verify_pair(destination))
        self.assertEqual((destination / "docs/guide.txt").read_bytes(), b"guide")
        self.assertEqual(
            (destination / "docs/tree/nested.txt").read_bytes(),
            (destination / "skills/tree/nested.txt").read_bytes(),
        )
        self.assertEqual(
            (destination / "skills/.agent/skills/rules/rule.txt").read_bytes(),
            b"rule",
        )
        self.assertEqual(
            (destination / "docs/TASK.txt").read_bytes(),
            (destination / "skills/TASK.txt").read_bytes(),
        )
        self.assertEqual(
            (destination / "docs/SETTINGS.json").read_bytes(),
            b'{"a":1,"z":[2,3]}',
        )
        self.assertEqual(
            (destination / "docs/SETTINGS.json").read_bytes(),
            (destination / "skills/SETTINGS.json").read_bytes(),
        )
        self.assertEqual(
            (destination / "docs/guide.txt").stat().st_ino
            != common_file.stat().st_ino,
            True,
        )
        self.assertEqual(
            set(item["path"] for item in manifest["treatment"]),
            {".agent/skills/rules/rule.txt"},
        )
        for arm_file in destination.glob("docs/*"):
            if arm_file.is_file():
                self.assertEqual(stat.S_IMODE(arm_file.stat().st_mode) & 0o222, 0)

        common_file.write_bytes(b"changed source")
        self.assertEqual((destination / "docs/guide.txt").read_bytes(), b"guide")
        self.assertEqual(manifest, materials.verify_pair(destination))

    def test_metadata_is_part_of_cohort(self) -> None:
        source = self._file("input", b"same")
        first = self.root / "first"
        second = self.root / "second"
        first_manifest = materials.freeze_pair(first, {"input": source}, {}, "one", {"answer": 1})
        second_manifest = materials.freeze_pair(second, {"input": source}, {}, "two", {"answer": 1})
        self.assertNotEqual(first_manifest["cohort_sha256"], second_manifest["cohort_sha256"])

    def test_settings_reject_nonfinite_and_non_json_values(self) -> None:
        source = self._file("input")
        for settings in (
            {"value": float("nan")},
            {"value": float("inf")},
            {"value": {1: "bad"}},
            {"value": object()},
            [],
            None,
        ):
            with self.subTest(settings=settings):
                destination = self.root / f"bad-{len(list(self.root.glob('bad-*')))}"
                with self.assertRaises(materials.MaterialError):
                    materials.freeze_pair(destination, {"input": source}, {}, "prompt", settings)
                self.assertFalse(os.path.lexists(destination))

    def test_symlink_and_special_source_entries_are_rejected_atomically(self) -> None:
        outside = self._file("outside", b"outside")
        link = self.root / "link"
        os.symlink(outside, link)
        destination = self.root / "symlink-pair"
        with self.assertRaises(materials.MaterialError):
            materials.freeze_pair(destination, {"input": link}, {}, "prompt", {})
        self.assertFalse(os.path.lexists(destination))

        tree = self.root / "tree"
        tree.mkdir()
        os.symlink(outside, tree / "escape")
        destination = self.root / "nested-symlink-pair"
        with self.assertRaises(materials.MaterialError):
            materials.freeze_pair(destination, {"tree": tree}, {}, "prompt", {})
        self.assertFalse(os.path.lexists(destination))

        if hasattr(os, "mkfifo"):
            fifo = self.root / "fifo"
            os.mkfifo(fifo)
            destination = self.root / "fifo-pair"
            with self.assertRaises(materials.MaterialError):
                materials.freeze_pair(destination, {"input": fifo}, {}, "prompt", {})
            self.assertFalse(os.path.lexists(destination))

    def test_mapping_traversal_reserved_and_overlap_are_rejected(self) -> None:
        source = self._file("input")
        bad_keys = (
            "../escape",
            "/absolute",
            ".",
            "a/../b",
            "a//b",
            "TASK.txt",
            "SETTINGS.json",
            ".agent",
            "a/.agent",
            "a/TASK.txt",
        )
        for index, key in enumerate(bad_keys):
            with self.subTest(key=key):
                destination = self.root / f"bad-key-{index}"
                with self.assertRaises(materials.MaterialError):
                    materials.freeze_pair(destination, {key: source}, {}, "prompt", {})
                self.assertFalse(os.path.lexists(destination))

        destination = self.root / "overlap"
        with self.assertRaises(materials.MaterialError):
            materials.freeze_pair(destination, {"a": source, "a/b": source}, {}, "prompt", {})
        self.assertFalse(os.path.lexists(destination))

    def test_mutation_during_copy_leaves_no_published_pair(self) -> None:
        source = self._file("input", b"original")
        destination = self.root / "mutating"
        original_read = materials._stable_read

        def read_then_mutate(path: Path) -> bytes:
            data = original_read(path)
            source.write_bytes(b"changed after read")
            return data

        with mock.patch.object(materials, "_stable_read", side_effect=read_then_mutate):
            with self.assertRaises(materials.MaterialError):
                materials.freeze_pair(destination, {"input": source}, {}, "prompt", {})
        self.assertFalse(os.path.lexists(destination))

    def test_verify_rejects_edit_missing_extra_and_manifest_tampering(self) -> None:
        destination, _ = self._pair()
        target = destination / "docs/input.txt"
        target.chmod(0o644)
        target.write_bytes(b"edited")
        with self.assertRaises(materials.MaterialError):
            materials.verify_pair(destination)

        destination, _ = self._pair()
        target = destination / "docs/input.txt"
        (destination / "docs").chmod(0o755)
        target.unlink()
        with self.assertRaises(materials.MaterialError):
            materials.verify_pair(destination)

        destination, _ = self._pair()
        docs = destination / "docs"
        docs.chmod(0o755)
        (docs / "extra.txt").write_bytes(b"extra")
        with self.assertRaises(materials.MaterialError):
            materials.verify_pair(destination)

        destination, _ = self._pair()
        manifest_path = destination / "manifest.json"
        manifest_path.chmod(0o644)
        manifest = json.loads(manifest_path.read_text())
        manifest["cohort_sha256"] = "0" * 64
        manifest_path.write_text(json.dumps(manifest, sort_keys=True, separators=(",", ":")))
        with self.assertRaises(materials.MaterialError):
            materials.verify_pair(destination)

        destination, _ = self._pair()
        manifest_path = destination / "manifest.json"
        manifest_path.chmod(0o644)
        with manifest_path.open("ab") as stream:
            stream.write(b" ")
        with self.assertRaises(materials.MaterialError):
            materials.verify_pair(destination)

    def test_verify_rejects_root_extra_and_artifact_symlink(self) -> None:
        destination, _ = self._pair()
        destination.chmod(0o755)
        (destination / "extra").write_bytes(b"extra")
        with self.assertRaises(materials.MaterialError):
            materials.verify_pair(destination)

        destination, _ = self._pair()
        docs = destination / "docs"
        docs.chmod(0o755)
        target = docs / "input.txt"
        target.unlink()
        os.symlink(self.root / "common/input.txt", target)
        with self.assertRaises(materials.MaterialError):
            materials.verify_pair(destination)

    def test_empty_treatment_directory_is_preserved(self) -> None:
        empty = self.root / "empty"
        empty.mkdir()
        common_empty = self.root / "common-empty"
        common_empty.mkdir()
        destination = self.root / "empty-pair"
        manifest = materials.freeze_pair(
            destination,
            {"empty": common_empty},
            {"empty": empty},
            "prompt",
            {},
        )
        self.assertEqual(manifest, materials.verify_pair(destination))
        self.assertTrue((destination / "skills/.agent/skills/empty").is_dir())
        self.assertTrue((destination / "docs/empty").is_dir())
        self.assertTrue((destination / "skills/empty").is_dir())

    def test_executable_bits_are_preserved_and_verified(self) -> None:
        source = self._file("script", b"#!/bin/sh")
        source.chmod(0o751)
        destination = self.root / "executable-pair"
        manifest = materials.freeze_pair(destination, {"script": source}, {}, "prompt", {})
        self.assertEqual(manifest["common"][0]["mode"], 0o111)
        self.assertEqual((destination / "docs/script").stat().st_mode & 0o111, 0o111)
        self.assertEqual((destination / "skills/script").stat().st_mode & 0o111, 0o111)
        self.assertEqual(materials.verify_pair(destination), manifest)

        target = destination / "docs/script"
        target.chmod(0o444)
        with self.assertRaises(materials.MaterialError):
            materials.verify_pair(destination)


if __name__ == "__main__":
    unittest.main()
