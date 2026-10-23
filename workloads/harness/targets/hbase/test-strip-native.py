#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from zipfile import ZipFile


class NativeResources(unittest.TestCase):
    def test_preserves_classes_and_removes_executable_resources(self):
        with tempfile.TemporaryDirectory() as directory:
            jar = Path(directory) / 'fixture.jar'
            entries = {'Example.class': b'\xca\xfe\xba\xbe\x00\x00\x00\x41',
                       'data.txt': b'configuration', 'native/lib.so': b'native',
                       'hidden/worker': b'\x7fELFpayload', 'windows/worker': b'MZpayload',
                       'native/lib.dylib': b'\xcf\xfa\xed\xfepayload'}
            with ZipFile(jar, 'w') as archive:
                for name, data in entries.items():
                    archive.writestr(name, data)
            subprocess.run([sys.executable, str(Path(__file__).with_name('strip-native.py')),
                            directory], check=True)
            with ZipFile(jar) as archive:
                self.assertEqual(set(archive.namelist()), {'Example.class', 'data.txt'})
                for name in archive.namelist():
                    self.assertEqual(archive.read(name), entries[name])


if __name__ == '__main__':
    unittest.main()
