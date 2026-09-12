#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("runtime_artifacts", Path(__file__).with_name("runtime-artifacts.py"))
RUNTIME = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNTIME)


class RuntimeArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name) / "source"
        self.repo.mkdir()
        for name in RUNTIME.INPUTS:
            path = self.repo / name
            if path.suffix:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("source")
            else:
                path.mkdir(parents=True, exist_ok=True)
        self.root = Path(self.temp.name) / "artifacts"
        for name in ("Image", "initramfs-oci.cpio.gz", "fixture/index.json", "fixture/oci-layout", "fixture/blobs/sha256/payload"):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("artifact")

    def test_exact_inputs_are_distinct_from_verified_old_artifacts(self):
        RUNTIME.seal(self.repo, self.root, "aarch64")
        self.assertEqual(RUNTIME.verify(self.repo, self.root, "aarch64")["scope"], "exact-input")
        (self.repo / "Cargo.lock").write_text("changed source")
        self.assertEqual(RUNTIME.verify(self.repo, self.root, "aarch64")["scope"], "host-only")

    def test_missing_manifest_never_qualifies_source(self):
        self.assertEqual(RUNTIME.verify(self.repo, self.root, "aarch64")["scope"], "inconclusive")
        self.assertEqual(RUNTIME.verify(self.repo, self.root / "absent", "aarch64")["scope"], "unavailable")

    def test_corrupt_or_extra_artifacts_fail_verification(self):
        RUNTIME.seal(self.repo, self.root, "aarch64")
        blob = self.root / "fixture/blobs/sha256/payload"
        blob.write_text("corrupt")
        with self.assertRaisesRegex(ValueError, "contents"):
            RUNTIME.verify(self.repo, self.root, "aarch64")
        blob.write_text("artifact")
        (blob.parent / "extra").write_text("unexpected")
        with self.assertRaisesRegex(ValueError, "contents"):
            RUNTIME.verify(self.repo, self.root, "aarch64")

    def test_build_caches_do_not_change_source_identity(self):
        expected = RUNTIME.source_digest(self.repo)
        cache = self.repo / "consonance/harmony-linux/supervisor/target/cache"
        cache.parent.mkdir(parents=True)
        cache.write_text("generated")
        self.assertEqual(RUNTIME.source_digest(self.repo), expected)

    def test_wrong_architecture_and_missing_kernel_fail(self):
        RUNTIME.seal(self.repo, self.root, "aarch64")
        with self.assertRaisesRegex(ValueError, "architecture"):
            RUNTIME.verify(self.repo, self.root, "x86_64")
        (self.root / "Image").unlink()
        with self.assertRaises(ValueError):
            RUNTIME.verify(self.repo, self.root, "aarch64")


if __name__ == "__main__":
    unittest.main()
