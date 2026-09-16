#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import gzip
import stat
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


def fixture(code=b"\xc3", flags=5, needed=None, soname=None):
    payload = bytearray(code)
    count = 1
    if needed or soname:
        count = 2
        payload.extend(bytes(0x100 - len(payload)))
        strings = b"\0" + (needed or soname).encode() + b"\0"
        payload.extend(strings)
        payload.extend(bytes(0x200 - len(payload)))
        payload.extend(b"".join(struct.pack("<qQ", *v) for v in [(1 if needed else 14, 1), (5, 0x400100), (10, len(strings)), (0, 0)]))
    header = struct.pack("<16sHHIQQQIHHHHHH", b"\x7fELF\x02\x01\x01" + bytes(9), 2, 62, 1, 0x400000, 64, 0, 0, 64, 56, count, 0, 0, 0)
    header += struct.pack("<IIQQQQQQ", 1, flags, 0x1000, 0x400000, 0, len(payload), len(payload), 0x1000)
    if needed or soname:
        header += struct.pack("<IIQQQQQQ", 2, 4, 0x1200, 0x400200, 0, 64, 64, 8)
    return header + bytes(0x1000 - len(header)) + payload


def newc(entries):
    output = bytearray()
    for index, (name, mode, content, rdev) in enumerate(entries + [("TRAILER!!!", 0, b"", (0, 0))]):
        name = name.encode() + b"\0"
        fields = [index + 1, mode, 0, 0, 1, 0, len(content), 0, 0, *rdev, len(name), 0]
        output.extend(b"070701" + b"".join(f"{v:08x}".encode() for v in fields))
        output.extend(name)
        output.extend(bytes((-len(output)) % 4))
        output.extend(content)
        output.extend(bytes((-len(output)) % 4))
    return bytes(output)


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

    def scan(self):
        return a.inventory(self.root, os.environ.get("OBJDUMP", "objdump"))

    def baseline(self, report):
        return {"version": 2, "archive_sha256": report.get("archive_sha256"),
                "rootfs_sha256": report["rootfs_sha256"],
                "elf_sha256": {name: item["sha256"] for name, item in report["artifacts"].items()},
                "xstate": {name: {"xgetbv": [site["address"] for site in item["sites"] if site["mnemonic"] == "xgetbv"],
                                  "resolver_regions": []} for name, item in report["artifacts"].items()}}

    def admitted(self, report, data, baseline):
        return a.verify(report, data, baseline)

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

    def test_tlsdesc_relocation_tables(self):
        for address_tag, size_tag, stride in ((7, 8, 24), (17, 18, 16), (23, 2, 24)):
            for relocation, expected in ((8, False), (34, True), (35, True), (36, True)):
                with self.subTest(table=address_tag, relocation=relocation):
                    data = bytearray(fixture(soname="fixture.so"))
                    entries = [(address_tag, 0x400100), (size_tag, stride)]
                    if address_tag == 23:
                        entries.append((20, 7))
                    entries.append((0, 0))
                    table = b"".join(struct.pack("<qQ", *entry) for entry in entries)
                    data[0x1200:0x1240] = table + bytes(64 - len(table))
                    struct.pack_into("<QQ", data, 0x1100, 0, (1 << 32) | relocation)
                    self.assertEqual(a.elf(bytes(data))["tlsdesc"], expected)
                    struct.pack_into("<Q", data, 0x1218, stride - 1)
                    with self.assertRaisesRegex(a.Rejected, "relocation table size"):
                        a.elf(bytes(data))

    def test_missing_dependency(self):
        self.binary.write_bytes(fixture(needed="libfixture.so"))
        with self.assertRaisesRegex(a.Rejected, "missing ELF dependency"):
            self.scan()

    def test_dependency_symlink_and_full_inventory(self):
        self.binary.write_bytes(fixture(needed="libfixture.so"))
        (self.root / "lib").mkdir()
        (self.root / "hidden").mkdir()
        (self.root / "hidden/library").write_bytes(fixture())
        (self.root / "lib/libfixture.so").symlink_to("/hidden/library")
        report, data = self.scan()
        dep = report["artifacts"]["/unexecutable"]["dependencies"][0]
        self.assertEqual(dep["resolved"], "/hidden/library")
        self.assertEqual(dep["symlinks"], [{"path": "/lib/libfixture.so", "target": "/hidden/library"}])
        baseline = self.baseline(report)
        del baseline["elf_sha256"]["/hidden/library"]
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
        with self.assertRaisesRegex(a.Rejected, "unsupported dynamic interpreter"):
            self.scan()

    def dynamic_tree(self):
        data = bytearray(fixture(needed="libfixture.so"))
        interpreter = a.GLIBC_INTERPRETER.encode() + b"\0"
        struct.pack_into("<H", data, 56, 3)
        struct.pack_into("<IIQQQQQQ", data, 176, 3, 4, len(data), 0, 0, len(interpreter), len(interpreter), 1)
        data.extend(interpreter)
        self.binary.write_bytes(data)
        (self.root / "lib64").mkdir()
        (self.root / "lib64/ld-linux-x86-64.so.2").write_bytes(fixture(soname="ld-linux-x86-64.so.2"))
        (self.root / "lib").mkdir()
        (self.root / "lib/libfixture.so").write_bytes(fixture())

    def test_glibc_requires_exact_loader_digest(self):
        self.dynamic_tree()
        report, data = self.scan()
        baseline = self.baseline(report)
        self.assertTrue(self.admitted(report, data, baseline))
        baseline["elf_sha256"][a.GLIBC_INTERPRETER] = "wrong"
        self.assertFalse(self.admitted(report, data, baseline))

    def test_missing_transitive_dependency(self):
        self.dynamic_tree()
        (self.root / "lib/libfixture.so").write_bytes(fixture(needed="libmissing.so"))
        with self.assertRaisesRegex(a.Rejected, "missing ELF dependency"):
            self.scan()

    def test_loader_cache_rejected(self):
        self.dynamic_tree()
        (self.root / "etc").mkdir()
        (self.root / "etc/ld.so.cache").write_bytes(b"cache")
        with self.assertRaisesRegex(a.Rejected, "loader cache"):
            self.scan()

    def test_ambiguous_library_rejected(self):
        self.dynamic_tree()
        (self.root / "usr/lib").mkdir(parents=True)
        (self.root / "usr/lib/libfixture.so").write_bytes(fixture(b"\x90\xc3"))
        with self.assertRaisesRegex(a.Rejected, "ambiguous differing"):
            self.scan()

    def test_differing_duplicate_sonames_rejected(self):
        self.binary.write_bytes(fixture(soname="libsame.so"))
        (self.root / "elsewhere").write_bytes(fixture(b"\x90\xc3", soname="libsame.so"))
        with self.assertRaisesRegex(a.Rejected, "differing ELF SONAME"):
            self.scan()

    def test_no_needed_dynamic_paths_rejected(self):
        for tag in (15, 29):
            data = bytearray(fixture(soname="/lib"))
            struct.pack_into("<q", data, 0x1200, tag)
            self.binary.write_bytes(data)
            with self.assertRaisesRegex(a.Rejected, "RPATH/RUNPATH"):
                self.scan()
        self.binary.write_bytes(fixture(soname="libfixture.so"))
        (self.root / "etc").mkdir()
        (self.root / "etc/ld.so.cache").symlink_to("/does-not-exist")
        with self.assertRaisesRegex(a.Rejected, "loader cache"):
            self.scan()

    def test_straightline_xgetbv_proof(self):
        cases = [
            (b"\x31\xc9\xb8\x01\0\0\0\x09\xd0\x0f\x01\xd0\xc3", True),
            (b"\x31\xc9\xb9\x01\0\0\0\x0f\x01\xd0\xc3", False),
            (b"\x31\xc9\xb1\x01\x0f\x01\xd0\xc3", False),
            (b"\x31\xc9\xeb\x00\x0f\x01\xd0\xc3", False),
            (b"\x31\xc9\xe8\0\0\0\0\x0f\x01\xd0\xc3", False),
            (b"\xeb\x02\x31\xc9\xb8\x01\0\0\0\x0f\x01\xd0\xc3", False),
        ]
        for code, accepted in cases:
            with self.subTest(code=code.hex()):
                self.binary.write_bytes(fixture(code))
                report, data = self.scan()
                self.assertEqual(self.admitted(report, data, self.baseline(report)), accepted)

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

    def test_resolver_exception_bound_to_bytes(self):
        self.binary.write_bytes(fixture(b"\x0f\xae\x27\xc3"))
        report, data = self.scan()
        baseline = self.baseline(report)
        region = {"kind": "eager-resolver", "start": 0x400000, "size": 3, "sha256": a.digest(b"\x0f\xae\x27")}
        baseline["xstate"]["/unexecutable"]["resolver_regions"] = [region]
        self.assertTrue(self.admitted(report, data, baseline))
        region["sha256"] = "wrong"
        self.assertFalse(self.admitted(report, data, baseline))

    def initramfs(self, device_minor=3):
        return newc([("bin", stat.S_IFDIR | 0o755, b"", (0, 0)),
                     ("bin/app", stat.S_IFREG | 0o4755, fixture(), (0, 0)),
                     ("bin/sh", stat.S_IFLNK | 0o777, b"/bin/app", (0, 0)),
                     ("dev", stat.S_IFDIR | 0o755, b"", (0, 0)),
                     ("dev/null", stat.S_IFCHR | 0o666, b"", (1, device_minor))])

    def test_initramfs_devices_metadata_and_archive_binding(self):
        archive = self.base / "initramfs.gz"
        archive.write_bytes(gzip.compress(self.initramfs(), mtime=0))
        report, data = a.inventory_initramfs(archive)
        files = {f["path"]: f for f in report["files"]}
        self.assertEqual(files["/dev/null"]["rdev"], [1, 3])
        self.assertEqual(files["/bin/app"]["mode"], 0o4755)
        self.assertEqual(files["/bin/sh"]["target"], "/bin/app")
        baseline = self.baseline(report)
        baseline["archive_sha256"] = None
        self.assertFalse(self.admitted(report, data, baseline))
        baseline["archive_sha256"] = report["archive_sha256"]
        self.assertTrue(self.admitted(report, data, baseline))
        archive.write_bytes(gzip.compress(self.initramfs(5), mtime=0))
        changed, data = a.inventory_initramfs(archive)
        self.assertNotEqual(changed["rootfs_sha256"], report["rootfs_sha256"])
        self.assertFalse(self.admitted(changed, data, baseline))

    def test_newc_duplicates_truncation_and_traversal(self):
        with self.assertRaisesRegex(a.Rejected, "duplicate"):
            a.parse_newc(newc([("a", stat.S_IFREG | 0o644, b"", (0, 0)), ("./a", stat.S_IFREG | 0o644, b"", (0, 0))]))
        for name in ("../escape", "/absolute", "a/../../escape"):
            with self.assertRaises(a.Rejected):
                a.parse_newc(newc([(name, stat.S_IFREG | 0o644, b"", (0, 0))]))
        for truncated in (self.initramfs()[:20], self.initramfs()[:-1]):
            with self.assertRaises(a.Rejected):
                a.parse_newc(truncated)
        with self.assertRaisesRegex(a.Rejected, "concatenated"):
            a.parse_newc(self.initramfs() + self.initramfs())

    def test_newc_symlink_parent_is_not_extracted(self):
        archive = self.base / "unsafe.cpio"
        archive.write_bytes(newc([("outside", stat.S_IFLNK | 0o777, b"/tmp", (0, 0)),
                                 ("outside/payload", stat.S_IFREG | 0o644, fixture(), (0, 0))]))
        with self.assertRaisesRegex(a.Rejected, "not a directory"):
            a.inventory_initramfs(archive)

    def test_text_relocations_rejected(self):
        for tag, value in ((22, 0), (30, 4)):
            with self.subTest(tag=tag):
                data = bytearray(fixture(soname="libfixture.so"))
                struct.pack_into("<qQ", data, 0x1200, tag, value)
                self.binary.write_bytes(data)
                report, contents = self.scan()
                self.assertTrue(report["artifacts"]["/unexecutable"]["text_relocations"])
                self.assertFalse(self.admitted(report, contents, self.baseline(report)))
                self.assertIn("text relocations", report["errors"][0])

    def test_tlsdesc_exception_requires_no_tlsdesc_relocations(self):
        self.binary.write_bytes(fixture(b"\x0f\xae\x27\xc3"))
        report, data = self.scan()
        baseline = self.baseline(report)
        region = {"kind": "unused-tlsdesc", "start": 0x400000, "size": 3,
                  "sha256": a.digest(b"\x0f\xae\x27")}
        baseline["xstate"]["/unexecutable"]["resolver_regions"] = [region]
        self.assertTrue(self.admitted(report, data, baseline))
        report["artifacts"]["/unexecutable"]["tlsdesc"] = True
        self.assertFalse(self.admitted(report, data, baseline))

    def test_review_attestations_are_not_contract_fields(self):
        report, data = self.scan()
        baseline = self.baseline(report)
        baseline["reviewed_by"] = "somebody"
        self.assertFalse(self.admitted(report, data, baseline))

    def test_newc_kernel_symlink_terminator(self):
        records = a.parse_newc(newc([("link", stat.S_IFLNK | 0o777, b"/bin/app\0", (0, 0))]))
        item, _ = records[0]
        self.assertEqual(item["target"], "/bin/app")
        self.assertEqual(item["sha256"], a.digest(b"/bin/app\0"))
        with self.assertRaisesRegex(a.Rejected, "symlink target"):
            a.parse_newc(newc([("link", stat.S_IFLNK | 0o777, b"/bin/app\0extra", (0, 0))]))

    def test_malformed_baseline(self):
        report, data = self.scan()
        self.assertFalse(self.admitted(report, data, [],))


if __name__ == "__main__":
    unittest.main()
