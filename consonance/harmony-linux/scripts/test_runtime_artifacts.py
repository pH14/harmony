#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

import importlib.util
import os
from pathlib import Path
import subprocess
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

    def test_default_cli_boot_contract_change_invalidates_old_pair(self):
        cli = self.repo / "cli/src/oci/runner.rs"
        cli.parent.mkdir(parents=True, exist_ok=True)
        (self.root / "bzImage").write_text("artifact")
        for architecture in ("x86_64", "aarch64"):
            with self.subTest(architecture=architecture):
                cli.write_text('x86 cmdline="harmony_pvclock"; arm cmdline="nohlt"')
                old_key = RUNTIME.source_digest(self.repo)
                RUNTIME.seal(self.repo, self.root, architecture)
                self.assertEqual(RUNTIME.verify(self.repo, self.root, architecture)["scope"], "exact-input")

                if architecture == "x86_64":
                    cli.write_text('x86 cmdline="tokenless"; arm cmdline="nohlt"')
                else:
                    cli.write_text('x86 cmdline="harmony_pvclock"; arm cmdline="new-arm-option"')
                self.assertNotEqual(RUNTIME.source_digest(self.repo), old_key)
                self.assertEqual(RUNTIME.verify(self.repo, self.root, architecture)["scope"], "host-only")

    def test_cli_boot_assembly_change_invalidates_old_pair(self):
        cli = self.repo / "cli/src/oci/mod.rs"
        cli.parent.mkdir(parents=True, exist_ok=True)
        (self.root / "bzImage").write_text("artifact")
        for architecture in ("x86_64", "aarch64"):
            with self.subTest(architecture=architecture):
                cli.write_text('default arguments: rdinit=/usr/lib/harmony/init')
                old_key = RUNTIME.source_digest(self.repo)
                RUNTIME.seal(self.repo, self.root, architecture)
                self.assertEqual(RUNTIME.verify(self.repo, self.root, architecture)["scope"], "exact-input")

                cli.write_text('default arguments: rdinit=/new/init')
                self.assertNotEqual(RUNTIME.source_digest(self.repo), old_key)
                self.assertEqual(RUNTIME.verify(self.repo, self.root, architecture)["scope"], "host-only")

    def test_cli_rebuild_watches_every_runtime_source_input(self):
        build_script = (Path(__file__).resolve().parents[3] / "cli/build.rs").read_text()
        for source in RUNTIME.INPUTS:
            with self.subTest(source=source):
                self.assertIn(f'"{source}"', build_script)

    def test_wrong_architecture_and_missing_kernel_fail(self):
        RUNTIME.seal(self.repo, self.root, "aarch64")
        with self.assertRaisesRegex(ValueError, "architecture"):
            RUNTIME.verify(self.repo, self.root, "x86_64")
        (self.root / "Image").unlink()
        with self.assertRaises(ValueError):
            RUNTIME.verify(self.repo, self.root, "aarch64")

    def test_canonical_build_rejects_profile_before_stale_default_can_be_sealed(self):
        (self.root / "bzImage").write_text("older default kernel")
        (self.root / "initramfs-oci.cpio.gz").write_text("new base")
        RUNTIME.seal(self.repo, self.root, "x86_64")
        self.assertEqual(RUNTIME.verify(self.repo, self.root, "x86_64")["scope"], "exact-input")

        tools = Path(self.temp.name) / "tools"
        tools.mkdir()
        uname = tools / "uname"
        uname.write_text('#!/bin/sh\ncase "$1" in -m) echo x86_64;; -s) echo Darwin;; esac\n')
        uname.chmod(0o755)
        for profile in ("N6_TRAPS_OFF", "TASK_PARK_PROFILE", "NESTED_HOST_PROFILE"):
            with self.subTest(profile=profile):
                environment = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}")
                for name in ("N6_TRAPS_OFF", "TASK_PARK_PROFILE", "NESTED_HOST_PROFILE", "ARM64_KERNEL_PROFILE"):
                    environment.pop(name, None)
                environment[profile] = "1"
                result = subprocess.run(["bash", str(Path(__file__).with_name("build-platform-runtime.sh"))],
                                        env=environment, capture_output=True, text=True, timeout=10)

                self.assertNotEqual(result.returncode, 0)
                self.assertIn("canonical platform runtime rejects profile kernels", result.stderr)

    def test_canonical_build_rejects_arm_profile_before_stale_image_can_be_sealed(self):
        (self.root / "Image").write_text("older default arm kernel")
        (self.root / "initramfs-oci.cpio.gz").write_text("new base")
        RUNTIME.seal(self.repo, self.root, "aarch64")
        self.assertEqual(RUNTIME.verify(self.repo, self.root, "aarch64")["scope"], "exact-input")
        original_manifest = (self.root / "runtime-manifest.json").read_bytes()

        tools = Path(self.temp.name) / "tools"
        tools.mkdir()
        uname = tools / "uname"
        uname.write_text('#!/bin/sh\ncase "$1" in -m) echo aarch64;; -s) echo Darwin;; esac\n')
        uname.chmod(0o755)
        environment = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}")
        for name in ("N6_TRAPS_OFF", "TASK_PARK_PROFILE", "NESTED_HOST_PROFILE"):
            environment.pop(name, None)
        environment["ARM64_KERNEL_PROFILE"] = "n6-traps-off"
        result = subprocess.run(["bash", str(Path(__file__).with_name("build-platform-runtime.sh"))],
                                env=environment, capture_output=True, text=True, timeout=10)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("canonical platform runtime rejects profile kernels", result.stderr)
        self.assertEqual((self.root / "Image").read_text(), "older default arm kernel")
        self.assertEqual((self.root / "runtime-manifest.json").read_bytes(), original_manifest)


if __name__ == "__main__":
    unittest.main()
