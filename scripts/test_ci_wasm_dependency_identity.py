# SPDX-License-Identifier: AGPL-3.0-or-later
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "wasm_dependency_identity", Path(__file__).with_name("wasm-dependency-identity.py")
)
IDENTITY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(IDENTITY)


class ConsumerDependencyIdentityTests(unittest.TestCase):
    def test_each_consumer_change_invalidates_the_shared_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index, name in enumerate(IDENTITY.LOCKFILES):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(f"locked dependency {index}\n".encode())
            original = IDENTITY.dependency_identity(root)
            for name in IDENTITY.LOCKFILES:
                path = root / name
                before = path.read_bytes()
                path.write_bytes(before + b"changed version or checksum\n")
                self.assertNotEqual(IDENTITY.dependency_identity(root), original, name)
                path.write_bytes(before)
                self.assertEqual(IDENTITY.dependency_identity(root), original)

    def test_missing_consumer_lockfile_cannot_produce_an_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(FileNotFoundError):
                IDENTITY.dependency_identity(Path(directory))


if __name__ == "__main__":
    unittest.main()
