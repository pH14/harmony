#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Freeze workload source identity without changing the registered experiment."""
import argparse
import hashlib
import json
from pathlib import Path


def source_hash(root, workspace=False):
    files = [path for path in root.iterdir() if path.is_file()
             and (path.suffix == '.rs' or path.name in ('Cargo.toml', 'Cargo.lock'))]
    files.extend((root / 'src').rglob('*.rs'))
    if workspace:
        files.append(root / '../Cargo.toml')
    digest = hashlib.sha256()
    for path in sorted(files):
        digest.update(str(path.relative_to(root)).encode())
        digest.update(b'\0')
        digest.update(path.read_bytes())
        digest.update(b'\0')
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--panel', type=Path, default=Path(__file__).with_name('panel.json'))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    panel = json.loads(args.panel.read_text())
    if panel['engine_source_sha256'] != source_hash(root / 'dissonance/searcher', True):
        raise ValueError('engine differs from pinned baseline; register that experiment separately')
    panel['workload_source_sha256'] = source_hash(root / 'workloads/tiny-worlds')
    args.panel.write_text(json.dumps(panel, indent=2) + '\n')
    print('Workload source identity frozen; seeds, instances, budgets and endpoints unchanged.')


if __name__ == '__main__':
    main()
