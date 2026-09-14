#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
import os
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("admission", Path(__file__).with_name("x86-xstate-admission.py"))
a = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(a)


def fixture(code=b"\xc3", flags=5, needed=None):
    payload = bytearray(code)
    count = 1
    if needed:
        count = 2
        payload.extend(bytes(0x100 - len(payload)))
        strings = b"\0" + needed.encode() + b"\0"
        payload.extend(strings)
        payload.extend(bytes(0x200 - len(payload)))
        payload.extend(b"".join(struct.pack("<qQ", *v) for v in [(1, 1), (5, 0x400100), (10, len(strings)), (0, 0)]))
    header = struct.pack("<16sHHIQQQIHHHHHH", b"\x7fELF\x02\x01\x01" + bytes(9), 2, 62, 1, 0x400000, 64, 0, 0, 64, 56, count, 0, 0, 0)
    header += struct.pack("<IIQQQQQQ", 1, flags, 0x1000, 0x400000, 0, len(payload), len(payload), 0x1000)
    if needed:
        header += struct.pack("<IIQQQQQQ", 2, 4, 0x1200, 0x400200, 0, 64, 64, 8)
    return header + bytes(0x1000 - len(header)) + payload


class AdmissionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / "root"
        self.root.mkdir()
        self.binary = self.root / "unexecutable"
        self.binary.write_bytes(fixture())
        self.binary.chmod(0o644)
        self.proof = self.base / "proof.md"
        self.proof.write_text("Test fixture review only: controlled entry and no indirect branches.\n")
        self.ref = {"file": "proof.md", "sha256": a.digest(self.proof.read_bytes())}

    def scan(self):
        return a.inventory(self.root, os.environ.get("OBJDUMP", "objdump"))

    def baseline(self, report):
        return {"version": 1, "reviewed_by": "unit-test fixture", "scope": a.SCOPE,
                "scope_evidence": self.ref, "rootfs_sha256": report["rootfs_sha256"],
                "artifacts": {name: {"sha256": item["sha256"], "review_evidence": self.ref,
                    "xgetbv": [{"address": site["address"], "selector": 0, "control_flow_evidence": self.ref}
                               for site in item["sites"] if site["mnemonic"] == "xgetbv"]}
                    for name, item in report["artifacts"].items()}}

    def admitted(self, report, data, baseline):
        return a.verify(report, data, baseline, self.base)

    def test_nonexecutable_elf_requires_review(self):
        report, data = self.scan()
        self.assertIn("/unexecutable", report["artifacts"])
        self.assertFalse(report["admitted"])
        self.assertFalse(self.admitted(report, data, {}))
        self.assertTrue(self.admitted(report, data, self.baseline(report)))

    def test_changed_hash_rejected_even_with_updated_tree_digest(self):
        old, _ = self.scan()
        baseline = self.baseline(old)
        self.binary.write_bytes(fixture(b"\x90\xc3"))
        report, data = self.scan()
        baseline["rootfs_sha256"] = report["rootfs_sha256"]
        self.assertFalse(self.admitted(report, data, baseline))
        self.assertIn("ELF digest differs", report["errors"][0])

    def test_new_save_requires_specific_review(self):
        for opcode in (b"\x0f\xae\x27", b"\x0f\xae\x37", b"\x0f\xc7\x27", b"\x0f\xc7\x2f", b"\x48\x0f\xae\x27"):
            with self.subTest(opcode=opcode.hex()):
                self.binary.write_bytes(fixture(opcode + b"\xc3"))
                report, data = self.scan()
                self.assertTrue(any(s["mnemonic"] in a.SAVE for s in report["artifacts"]["/unexecutable"]["sites"]))
                self.assertFalse(self.admitted(report, data, self.baseline(report)))

    def test_xgetbv_selector_and_incoming_branch(self):
        for code, accepted in [(b"\x31\xc9\x0f\x01\xd0\xc3", True), (b"\xb9\x01\0\0\0\x0f\x01\xd0\xc3", False), (b"\xeb\x02\x31\xc9\x0f\x01\xd0\xc3", False)]:
            with self.subTest(code=code.hex()):
                self.binary.write_bytes(fixture(code))
                report, data = self.scan()
                self.assertEqual(self.admitted(report, data, self.baseline(report)), accepted)

    def test_missing_dependency(self):
        self.binary.write_bytes(fixture(needed="libfixture.so"))
        with self.assertRaisesRegex(a.Rejected, "missing ELF dependency"):
            self.scan()

    def test_dependency_symlink_and_full_inventory(self):
        self.binary.write_bytes(fixture(needed="libfixture.so"))
        (self.root / "lib64").mkdir()
        (self.root / "hidden").mkdir()
        (self.root / "hidden/library").write_bytes(fixture())
        (self.root / "lib64/libfixture.so").symlink_to("/hidden/library")
        report, data = self.scan()
        dep = report["artifacts"]["/unexecutable"]["dependencies"][0]
        self.assertEqual(dep["resolved"], "/hidden/library")
        self.assertEqual(dep["symlinks"], [{"path": "/lib64/libfixture.so", "target": "/hidden/library"}])
        baseline = self.baseline(report)
        del baseline["artifacts"]["/hidden/library"]
        self.assertFalse(self.admitted(report, data, baseline))

    def test_writable_executable_rejected(self):
        self.binary.write_bytes(fixture(flags=7))
        report, data = self.scan()
        self.assertFalse(self.admitted(report, data, self.baseline(report)))
        self.assertIn("writable executable", report["errors"][0])

    def test_executable_stack_rejected(self):
        for flags, rejected in ((6, False), (7, True)):
            with self.subTest(flags=flags):
                data = bytearray(fixture())
                struct.pack_into("<H", data, 56, 2)
                struct.pack_into("<IIQQQQQQ", data, 120, 0x6474E551, flags, 0, 0, 0, 0, 0, 16)
                record = a.elf(bytes(data))
                self.assertEqual(record["executable_stack"], rejected)
                record.update(sha256=a.digest(data), sites=[], writable_executable_segments=[])
                report = {"rootfs_sha256": "fixture", "artifacts": {"/fixture": record}}
                self.assertEqual(self.admitted(report, {"/fixture": bytes(data)}, self.baseline(report)), not rejected)
                if rejected:
                    self.assertIn("executable ELF stack", report["errors"][0])

    def test_uid_gid_bound(self):
        report, _ = self.scan()
        entry = next(f for f in report["files"] if f["path"] == "/unexecutable")
        self.assertEqual(entry["uid"], self.binary.stat().st_uid)
        self.assertEqual(entry["gid"], self.binary.stat().st_gid)
        self.assertIn("xattrs", entry)

    @unittest.skipUnless(os.geteuid() == 0, "ownership mutation requires root")
    def test_ownership_changes_invalidate_review(self):
        report, _ = self.scan()
        baseline = self.baseline(report)
        os.chown(self.binary, self.binary.stat().st_uid, self.binary.stat().st_gid + 1)
        changed, contents = self.scan()
        self.assertNotEqual(report["rootfs_sha256"], changed["rootfs_sha256"])
        self.assertFalse(self.admitted(changed, contents, baseline))

    def test_xattr_changes_invalidate_review(self):
        report, _ = self.scan()
        baseline = self.baseline(report)
        try:
            os.setxattr(self.binary, "user.admission-test", b"changed")
        except OSError as error:
            self.skipTest(f"test filesystem lacks user xattrs: {error}")
        changed, contents = self.scan()
        self.assertNotEqual(report["rootfs_sha256"], changed["rootfs_sha256"])
        self.assertFalse(self.admitted(changed, contents, baseline))

    def test_unreadable_xattrs_rejected(self):
        with patch.object(os, "listxattr", side_effect=PermissionError("denied")):
            with self.assertRaisesRegex(a.Rejected, "extended attributes"):
                self.scan()

    def test_dynamic_interpreter_rejected(self):
        data = bytearray(fixture())
        interpreter = b"/lib/ld-musl-x86_64.so.1\0"
        struct.pack_into("<H", data, 56, 2)
        struct.pack_into("<IIQQQQQQ", data, 120, 3, 4, len(data), 0, 0, len(interpreter), len(interpreter), 1)
        data.extend(interpreter)
        self.binary.write_bytes(data)
        with self.assertRaisesRegex(a.Rejected, "dynamic interpreters are not modeled"):
            self.scan()

    def test_musl_configuration_rejected(self):
        self.binary.write_bytes(fixture(needed="libfixture.so"))
        (self.root / "etc").mkdir()
        (self.root / "etc/ld-musl-x86_64.path").write_text("/custom")
        with self.assertRaisesRegex(a.Rejected, "musl loader configuration"):
            self.scan()

    def test_elf_parser_rejects_truncation(self):
        with self.assertRaises(a.Rejected):
            a.elf(fixture()[:-1])
        self.assertEqual(a.elf(fixture())["segments"][0]["flags"], 5)

    def test_guest_symlink_cannot_escape(self):
        (self.root / "escape").symlink_to("../../outside")
        with self.assertRaisesRegex(a.Rejected, "escapes root"):
            a.resolve(self.root, "/escape")

    def test_read_failure_not_silently_skipped(self):
        original = Path.open
        def denied(path, *args, **kwargs):
            if path.resolve() == self.binary.resolve():
                raise PermissionError("fixture unreadable")
            return original(path, *args, **kwargs)
        with patch.object(Path, "open", denied):
            with self.assertRaises(PermissionError):
                self.scan()

    def test_resolver_proof_bound_to_bytes_and_files(self):
        self.binary.write_bytes(fixture(b"\x0f\xae\x27\xc3"))
        report, data = self.scan()
        baseline = self.baseline(report)
        baseline["artifacts"]["/unexecutable"]["resolver_regions"] = [{"kind": "reviewed-eager-resolver", "start": 0x400000, "size": 3, "sha256": a.digest(b"\x0f\xae\x27"), "incoming_reference_evidence": self.ref, "eager_binding_evidence": self.ref}]
        self.assertTrue(self.admitted(report, data, baseline))
        self.proof.write_text("changed")
        self.assertFalse(self.admitted(report, data, baseline))

    def test_malformed_baseline(self):
        report, data = self.scan()
        self.assertFalse(self.admitted(report, data, [],))


if __name__ == "__main__":
    unittest.main()
