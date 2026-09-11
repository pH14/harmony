# SPDX-License-Identifier: AGPL-3.0-or-later
"""Qualify byte and inode exhaustion on an empty, small Linux tmpfs mount."""
from __future__ import annotations

import argparse
import errno
import json
import os
from pathlib import Path

from .guest_limits import verify_output_mount


def qualify(parent: Path) -> dict:
    mount = verify_output_mount(parent)
    info = os.statvfs(parent)
    capacity = info.f_blocks * info.f_frsize
    if capacity > 8 * 1024**2 or info.f_files > 128 or any(parent.iterdir()):
        raise ValueError('storage canary requires an empty mount of at most 8 MiB and 128 inodes')
    checks = {}
    probe = parent / 'bytes'
    try:
        try:
            with probe.open('wb', buffering=0) as stream:
                for _ in range(capacity // 65536 + 2):
                    stream.write(bytes(65536))
        except OSError as error:
            if error.errno != errno.ENOSPC:
                raise
            checks['byte_exhaustion'] = True
        if not checks.get('byte_exhaustion'):
            raise ValueError('byte writes exceeded the advertised filesystem capacity')
    finally:
        probe.unlink(missing_ok=True)
    created = []
    try:
        try:
            for index in range(info.f_files + 1):
                probe = parent / ('inode-%d' % index)
                probe.touch(exist_ok=False)
                created.append(probe)
        except OSError as error:
            if error.errno != errno.ENOSPC:
                raise
            checks['inode_exhaustion'] = True
        if not checks.get('inode_exhaustion'):
            raise ValueError('file creation exceeded the advertised inode capacity')
    finally:
        for probe in created:
            probe.unlink()
    return {'qualified': True, 'model_calls': 0, 'mount': mount, 'checks': checks}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('parent', type=Path)
    args = parser.parse_args()
    try:
        result = qualify(args.parent)
    except Exception as error:
        result = {'qualified': False, 'model_calls': 0, 'error': str(error)}
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result['qualified'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
