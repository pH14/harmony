#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import copy
import gzip
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("prepared", Path(__file__).with_name("verify-prepared-admission.py"))
p = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(p)


def entry(path, kind="directory", content=b""):
    return ({"path": path, "kind": kind, "mode": 0o755, "uid": 0, "gid": 0,
             "inode": 1, "nlink": 1, "mtime": 0, "archive_device": [0, 0], "rdev": [0, 0]}, content)


class CompositionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.execution = {"argv": ["/opt/harmony/play-agent", "--nes-payload", "--rom", "/game.nes"], "env": ["LD_BIND_NOW=1"]}
        self.config = {"process": {"args": ["/usr/lib/harmony/supervisor"], "env": ["LD_BIND_NOW=1"]},
                       "root": {"path": "rootfs", "readonly": False},
                       "mounts": [{"destination": "/game.nes", "source": "/harmony-oci/external/game.nes", "options": ["bind", "ro"]}]}
        self.rom = b"NES\x1a" + bytes(12)
        self.platform = [entry("/init", "file", b"init")]
        self.payload = [entry("/harmony-oci"), entry("/harmony-oci/rootfs"), entry("/harmony-oci/rootfs/opt"), entry("/harmony-oci/rootfs/opt/harmony"), entry("/harmony-oci/rootfs/opt/harmony/play-agent", "file", b"agent")]
        self.expected_rom = {"sha256": p.a.digest(self.rom), "size": len(self.rom)}
        self.write_dump()

    def write_dump(self, extra_control=None):
        execution = json.dumps(self.execution).encode()
        control = [entry("/harmony-oci"), entry("/harmony-oci/config.json", "file", json.dumps(self.config).encode()), entry("/harmony-oci/execution.json", "file", execution), entry("/harmony-oci/external"), entry("/harmony-oci/external/game.nes", "file", self.rom)]
        if extra_control:
            control.append(extra_control)
        blobs = {"kernel.bin": b"kernel", "execution.json": execution,
                 "image-runtime-config.json": b"{}", "image-inputs.json": b"{}",
                 "session-config.json": json.dumps({"cmdline": "LD_BIND_NOW=1 noxsaveopt noxsaves rdinit=/init"}).encode()}
        for name, entries in zip(p.COMPONENTS, [self.platform, self.payload, control]):
            blobs[name] = gzip.compress(p.encode_newc(entries), mtime=0)
        blobs["composed-initramfs.bin"] = b"".join(blobs[n] for n in p.COMPONENTS)
        digest = hashlib.sha256(b"harmony-oci-prepared-execution-v1\0")
        for name in ["rootfs.cpio.gz", "control.cpio.gz", "execution.json"]:
            digest.update(struct.pack("<Q", len(blobs[name])))
            digest.update(blobs[name])
        offset, order = 0, []
        for name in p.COMPONENTS:
            order.append({"file": name, "offset": offset, "size": len(blobs[name])})
            offset += len(blobs[name])
        manifest = {"version": 1, "mode": "nes", "engine_scope": "default-linux-x86_64-kvm-session", "files": {n: {"sha256": p.a.digest(d), "size": len(d)} for n, d in blobs.items()}, "composition": order, "prepared_identity": digest.hexdigest(), "external_inputs": {"/game.nes": self.expected_rom}, "code_inputs": {}}
        for name, data in blobs.items():
            (self.root / name).write_bytes(data)
        (self.root / "manifest.json").write_text(json.dumps(manifest))

    def oracle_dump(self):
        manifest_path = self.root / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        session = {"ram_bytes": 134217728, "seed": p.ORACLE_SEED,
                   "run_budget": 2000000000, "cmdline": p.ORACLE_CMDLINE,
                   "identity_tag": "", "wall_limit": None,
                   "defer_virtual_time_checkpoint_hashes": True}
        data = json.dumps(session).encode()
        (self.root / "session-config.json").write_bytes(data)
        manifest["files"]["session-config.json"] = {"size": len(data), "sha256": p.a.digest(data)}
        manifest["engine_scope"] = p.ORACLE_SCOPE
        manifest["oracle"] = {"version": 1, "backend": "boot_linux_stock_virtual_time",
            "control": "direct-ControlServer-A-E", "restore_mode": "in-place-with-remap-factory",
            "deadline_kind": "absolute-vtime", "setup_payloads": [[0, 1]] * 16,
            "tree_seed": p.ORACLE_SEED ^ 0x4954454d325f5452}
        manifest_path.write_text(json.dumps(manifest))
        return manifest, session

    def test_oracle_candidate_is_distinct_and_not_admitted(self):
        self.oracle_dump()
        report, _ = p.inspect_dump(self.root)
        self.assertFalse(report["admitted"])
        self.assertEqual(report["engine_scope"], p.ORACLE_SCOPE)

    def test_resealed_oracle_configuration_mutations(self):
        for field, value in [("seed", 0), ("ram_bytes", 268435456),
                             ("run_budget", 1), ("defer_virtual_time_checkpoint_hashes", False),
                             ("cmdline", p.ORACLE_CMDLINE + " quiet")]:
            with self.subTest(field=field):
                manifest, session = self.oracle_dump()
                session[field] = value
                data = json.dumps(session).encode()
                (self.root / "session-config.json").write_bytes(data)
                manifest["files"]["session-config.json"] = {"size": len(data), "sha256": p.a.digest(data)}
                (self.root / "manifest.json").write_text(json.dumps(manifest))
                with self.assertRaisesRegex(p.a.Rejected, "oracle configuration"):
                    p.inspect_dump(self.root)

    def test_oracle_control_and_scope_mutations(self):
        for field, value in [("tree_seed", 0), ("restore_mode", "cold"),
                             ("deadline_kind", "relative"), ("setup_payloads", [[0, 1]])]:
            with self.subTest(field=field):
                manifest, _ = self.oracle_dump()
                manifest["oracle"][field] = value
                (self.root / "manifest.json").write_text(json.dumps(manifest))
                with self.assertRaisesRegex(p.a.Rejected, "oracle controls"):
                    p.inspect_dump(self.root)
        manifest, _ = self.oracle_dump()
        manifest["engine_scope"] = "default-linux-x86_64-kvm-session"
        (self.root / "manifest.json").write_text(json.dumps(manifest))
        with self.assertRaisesRegex(p.a.Rejected, "cannot use default"):
            p.inspect_dump(self.root)

    def test_verification_admits_only_components_with_clean_properties(self):
        self.oracle_dump()
        clean = {"interpreter": None, "needed": [], "soname": None, "tlsdesc": False,
                 "text_relocations": False, "executable_stack": False,
                 "writable_executable_segments": [], "sites": []}
        for writable, admitted in ((False, True), (True, False)):
            with self.subTest(writable=writable), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "report"
                argv = ["verify-prepared-admission.py", "verify", str(self.root),
                        "--output", str(output)]
                record = dict(clean, writable_executable_segments=[{}] if writable else [])
                scan = ({"artifacts": {"/binary": record}}, {})
                with mock.patch("sys.argv", argv), mock.patch.object(p.a, "inventory_initramfs", return_value=scan):
                    self.assertEqual(p.main(), 0 if admitted else 1)
                report = json.loads((output / "composition-report.json").read_text())
                self.assertEqual(report["admitted"], admitted)
                if not admitted:
                    self.assertIn("writable executable ELF segment", report["errors"][0])

    def test_component_projection_and_candidate_only(self):
        report, data = p.inspect_dump(self.root)
        self.assertFalse(report["admitted"])
        paths = [i["path"] for i, _ in p.a.parse_newc(data)]
        self.assertIn("/opt/harmony/play-agent", paths)
        self.assertNotIn("/harmony-oci/rootfs/opt/harmony/play-agent", paths)

    def test_changed_dump_bytes(self):
        (self.root / "kernel.bin").write_bytes(b"different")
        with self.assertRaisesRegex(p.a.Rejected, "hash/size"):
            p.inspect_dump(self.root)

    def test_composition_order(self):
        path = self.root / "manifest.json"
        manifest = json.loads(path.read_text())
        manifest["composition"].reverse()
        path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(p.a.Rejected, "composition order"):
            p.inspect_dump(self.root)

    def test_changed_rom_after_resealed_components(self):
        self.rom += b"changed"
        self.write_dump()
        with self.assertRaisesRegex(p.a.Rejected, "external input digest"):
            p.inspect_dump(self.root)

    def test_loader_environment_after_resealed_components(self):
        self.execution["env"].append("LD_PRELOAD=/evil.so")
        self.write_dump()
        with self.assertRaisesRegex(p.a.Rejected, "loader environment"):
            p.inspect_dump(self.root)

    def test_external_mount_must_be_readonly(self):
        self.config["mounts"][0]["options"] = ["bind", "rw"]
        self.write_dump()
        with self.assertRaisesRegex(p.a.Rejected, "read-only"):
            p.inspect_dump(self.root)

    def test_file_overlay_collision(self):
        self.platform.append(copy.deepcopy(self.payload[-1]))
        self.write_dump()
        with self.assertRaisesRegex(p.a.Rejected, "namespace|archive collision"):
            p.inspect_dump(self.root)

    def test_platform_cannot_inject_unlisted_workload_code(self):
        self.platform.append(entry("/harmony-oci/rootfs/unlisted", "file", b"code"))
        self.write_dump()
        with self.assertRaisesRegex(p.a.Rejected, "namespace"):
            p.inspect_dump(self.root)

    def test_boot_environment_after_resealed_manifest(self):
        path = self.root / "session-config.json"
        path.write_text(json.dumps({"cmdline": "noxsaveopt noxsaves rdinit=/init"}))
        manifest_path = self.root / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["files"][path.name] = {"size": path.stat().st_size, "sha256": p.a.digest(path.read_bytes())}
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(p.a.Rejected, "pre-PID1"):
            p.inspect_dump(self.root)

    def test_control_code_injection(self):
        self.write_dump(entry("/harmony-oci/unreviewed", "file", b"code"))
        with self.assertRaisesRegex(p.a.Rejected, "control input"):
            p.inspect_dump(self.root)


if __name__ == "__main__":
    unittest.main()
