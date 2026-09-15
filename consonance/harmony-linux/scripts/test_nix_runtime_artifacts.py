#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("bridge", Path(__file__).with_name("nix-runtime-artifacts.py"))
BRIDGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BRIDGE)


class BridgeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.repo = self.base / "repo"
        for name in BRIDGE.runtime.INPUTS:
            path = self.repo / name
            if path.suffix:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("source")
            else:
                path.mkdir(parents=True, exist_ok=True)
        self.root = self.base / "nix"
        for name in ["x86_64/bzImage", "x86_64/initramfs-oci.cpio.gz", "x86_64/initramfs.cpio.gz", "x86_64/oci-runtime.manifest", "MANIFEST.sha256", BRIDGE.PAYLOAD_MANIFEST]:
            p = self.root / name
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(name)
        self.fixture = self.base / "fixture"
        for name in ["index.json", "oci-layout", "blobs/sha256/test"]:
            p = self.fixture / name
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text("fixture")
        BRIDGE.record(self.repo, self.root, "x86_64")
        self.output = self.base / "out"

    def payloads(self):
        source = self.repo / "consonance/harmony-linux/runtime/init.sh"
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_text("canonical init")
        payload = self.base / "payload"
        payload.mkdir()
        (payload / "init.sh").write_bytes(source.read_bytes())
        (payload / "harmony-supervisor").write_text("built supervisor")
        BRIDGE.payload_record(self.repo, payload, "x86_64")
        return payload

    def test_payload_source_and_bytes_binding(self):
        p = self.payloads()
        BRIDGE.payload_verify(self.repo, p / BRIDGE.PAYLOAD_MANIFEST, p / "init.sh", p / "harmony-supervisor", "x86_64")
        (p / "harmony-supervisor").write_text("stale supervisor")
        with self.assertRaisesRegex(ValueError, "payload bytes"):
            BRIDGE.payload_verify(self.repo, p / BRIDGE.PAYLOAD_MANIFEST, p / "init.sh", p / "harmony-supervisor", "x86_64")

    def test_stale_payload_source_rejected(self):
        p = self.payloads()
        (self.repo / "Cargo.lock").write_text("changed")
        with self.assertRaisesRegex(ValueError, "payload source"):
            BRIDGE.payload_verify(self.repo, p / BRIDGE.PAYLOAD_MANIFEST, p / "init.sh", p / "harmony-supervisor", "x86_64")

    def package(self):
        BRIDGE.package(self.repo, self.root, self.output, self.fixture, "x86_64")

    def test_package_preserves_bytes_and_seals_provenance(self):
        self.package()
        for name in ["bzImage", "initramfs-oci.cpio.gz", "initramfs.cpio.gz"]:
            self.assertEqual((self.output / name).read_bytes(), (self.root / "x86_64" / name).read_bytes())
        self.assertEqual(BRIDGE.runtime.verify(self.repo, self.output, "x86_64")["scope"], "exact-input")
        (self.output / "build-provenance/MANIFEST.sha256").write_text("mutated")
        with self.assertRaises(ValueError):
            BRIDGE.runtime.verify(self.repo, self.output, "x86_64")

    def test_missing_direct_fixture_is_unavailable(self):
        (self.root / "x86_64/initramfs.cpio.gz").unlink()
        with self.assertRaisesRegex(ValueError, "unavailable.*direct fixture"):
            BRIDGE.record(self.repo, self.root, "x86_64")
        with self.assertRaises(ValueError):
            self.package()
        self.assertFalse(self.output.exists())

    def test_direct_fixture_mutation_invalidates_canonical_manifest(self):
        self.package()
        (self.output / "initramfs.cpio.gz").write_text("changed")
        with self.assertRaisesRegex(ValueError, "contents"):
            BRIDGE.runtime.verify(self.repo, self.output, "x86_64")

    def test_stale_source_rejected_before_output(self):
        (self.repo / "Cargo.lock").write_text("new source")
        with self.assertRaisesRegex(ValueError, "source digest"):
            self.package()
        self.assertFalse(self.output.exists())

    def test_mutated_prebuilt_rejected(self):
        (self.root / "x86_64/bzImage").write_text("different kernel")
        with self.assertRaisesRegex(ValueError, "bytes differ"):
            self.package()
        self.assertFalse(self.output.exists())

    def test_original_manifest_mutation_rejected(self):
        (self.root / "MANIFEST.sha256").write_text("changed manifest")
        with self.assertRaisesRegex(ValueError, "bytes differ"):
            self.package()

    def test_extra_prebuilt_file_rejected(self):
        (self.root / "extra").write_text("unrecorded")
        with self.assertRaisesRegex(ValueError, "bytes differ"):
            self.package()

    def test_old_output_without_provenance_rejected(self):
        (self.root / BRIDGE.PROVENANCE).unlink()
        with self.assertRaises(OSError):
            self.package()

    def test_symlink_rejected(self):
        (self.root / "extra").symlink_to(self.repo / "Cargo.lock")
        with self.assertRaisesRegex(ValueError, "unsupported"):
            self.package()


if __name__ == "__main__":
    unittest.main()
