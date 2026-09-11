# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import hashlib
import io
import json
import os
import tarfile
import unittest

try:
    from . import build, guest_image
except ImportError:  # unittest discovery can load this directory as top-level.
    import build  # type: ignore[no-redef]
    import guest_image  # type: ignore[no-redef]


def artifact(path: str, data: bytes, executable: bool = False) -> build.Artifact:
    return build.Artifact(path, data, hashlib.sha256(data).hexdigest(), executable)


class GuestImageTests(unittest.TestCase):
    def package(self, extra: tuple[build.Artifact, ...] = ()) -> bytes:
        return guest_image.package_artifacts(
            (artifact("app", b"ELF", True), artifact("bin/helper", b"helper"), *extra),
            b"controller bundle",
        )

    def unpack(self, image: bytes) -> tuple[dict, dict, list[tarfile.TarInfo], dict[str, bytes]]:
        with tarfile.open(fileobj=io.BytesIO(image), mode="r:") as outer:
            members = outer.getmembers()
            self.assertEqual([member.name for member in members], sorted(member.name for member in members))
            self.assertEqual({member.name for member in members}, {"manifest.json", "layer.tar", next(name for name in (member.name for member in members) if name.endswith(".json") and name != "manifest.json")})
            for member in members:
                self.assertTrue(member.isreg())
                self.assertEqual((member.uid, member.gid, member.uname, member.gname, member.mtime), (0, 0, "", "", 0))
            manifest = json.loads(outer.extractfile("manifest.json").read())  # type: ignore[union-attr]
            config_name = manifest[0]["Config"]
            config = json.loads(outer.extractfile(config_name).read())  # type: ignore[union-attr]
            layer = outer.extractfile("layer.tar").read()  # type: ignore[union-attr]
        with tarfile.open(fileobj=io.BytesIO(layer), mode="r:") as inner:
            inner_members = inner.getmembers()
            files = {
                member.name: inner.extractfile(member).read()  # type: ignore[union-attr]
                for member in inner_members
                if member.isreg()
            }
            for member in inner_members:
                self.assertIn(member.type, (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE))
                self.assertEqual((member.uid, member.gid, member.uname, member.gname, member.mtime), (0, 0, "", "", 0))
        return manifest, config, inner_members, files

    def test_archive_is_repeatable_and_has_actual_layer_digest(self) -> None:
        first = self.package()
        self.assertEqual(first, self.package())
        manifest, config, members, files = self.unpack(first)
        self.assertEqual(manifest[0]["Layers"], ["layer.tar"])
        self.assertEqual(config["architecture"], "amd64")
        self.assertEqual(config["os"], "linux")
        self.assertEqual(config["config"], {"Entrypoint": ["/app/app"], "Env": [], "WorkingDir": "/"})
        self.assertIn("app/app", files)
        self.assertIn("app/bin/helper", files)
        self.assertEqual(files["etc/harmony/bundle"], b"controller bundle")
        modes = {member.name: member.mode for member in members}
        self.assertEqual(modes["app"], 0o755)
        self.assertEqual(modes["app/app"], 0o755)
        self.assertEqual(modes["app/bin/helper"], 0o644)
        self.assertEqual(modes["etc/harmony/bundle"], 0o644)
        with tarfile.open(fileobj=io.BytesIO(first), mode="r:") as outer:
            layer = outer.extractfile("layer.tar").read()  # type: ignore[union-attr]
        digest = hashlib.sha256(layer).hexdigest()
        self.assertEqual(config["rootfs"]["diff_ids"], ["sha256:" + digest])

    def test_changed_artifact_bytes_change_archive(self) -> None:
        original = self.package()
        changed = guest_image.package_artifacts(
            (artifact("app", b"different", True), artifact("bin/helper", b"helper")),
            b"controller bundle",
        )
        self.assertNotEqual(original, changed)

    def test_validation_rejects_digest_types_paths_overlap_and_limits(self) -> None:
        valid = artifact("app", b"x")
        cases = (
            (build.Artifact("app", b"x", "0" * 64, False), b"bundle"),
            (build.Artifact("app", bytearray(b"x"), hashlib.sha256(b"x").hexdigest(), False), b"bundle"),
            (build.Artifact("../app", b"x", hashlib.sha256(b"x").hexdigest(), False), b"bundle"),
            (artifact("x" * (guest_image.MAX_PATH_COMPONENT_BYTES + 1), b"x"), b"bundle"),
            (artifact(".wh.deleted", b"x"), b"bundle"),
            (artifact("nested/.wh.deleted", b"x"), b"bundle"),
            ((valid, valid), b"bundle"),
            ((valid, artifact("app/nested", b"y")), b"bundle"),
            ((valid,), b"x" * (guest_image.MAX_BUNDLE_BYTES + 1)),
            ((), b"bundle"),
        )
        for artifacts, bundle in cases:
            with self.subTest(artifacts=artifacts):
                with self.assertRaises(guest_image.GuestImageError):
                    guest_image.package_artifacts(artifacts, bundle)  # type: ignore[arg-type]
        with self.assertRaises(guest_image.GuestImageError):
            guest_image.package_artifacts((valid,), bytearray(b"bundle"))  # type: ignore[arg-type]

    def test_long_control_name_remains_regular_and_deterministic(self) -> None:
        name = "nested/" + "\n".join(["control"] * 30) + "/leaf"
        image = guest_image.package_artifacts((artifact(name, b"payload"),), b"bundle")
        _, _, members, files = self.unpack(image)
        self.assertEqual(files["app/" + name], b"payload")
        target = next(member for member in members if member.name == "app/" + name)
        self.assertTrue(target.isreg())
        self.assertFalse(target.issym() or target.islnk())


if __name__ == "__main__":
    unittest.main()
