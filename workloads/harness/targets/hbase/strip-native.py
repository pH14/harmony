#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Remove bundled native libraries so the test must use Java implementations."""
from pathlib import Path
import sys
import zipfile

for root in sys.argv[1:]:
    for jar in sorted(Path(root).rglob('*.jar')):
        with zipfile.ZipFile(jar) as source:
            native = []
            for entry in source.infolist():
                if entry.is_dir():
                    continue
                with source.open(entry) as stream:
                    magic = stream.read(4)
                name = entry.filename.lower()
                if (name.endswith(('.so', '.dll', '.dylib', '.jnilib')) or '.so.' in name
                        or magic == b'\x7fELF' or magic[:2] == b'MZ'
                        or magic in (b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xca\xfe\xba\xbe')
                        and not name.endswith('.class')):
                    native.append(entry.filename)
            if not native:
                continue
            temporary = jar.with_suffix('.rewritten')
            with zipfile.ZipFile(temporary, 'w', zipfile.ZIP_DEFLATED) as target:
                for entry in source.infolist():
                    if entry.filename not in native:
                        target.writestr(entry, source.read(entry.filename))
        temporary.replace(jar)
        print(f'{jar}: removed {native}', flush=True)
