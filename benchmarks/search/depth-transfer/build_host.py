#!/usr/bin/env python3
"""Attest native binaries from an immutable source tree on a second host."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--source-commit', required=True)
    parser.add_argument('--source-sha256', required=True)
    args = parser.parse_args()
    root = args.root.resolve()
    source = root / 'source-milestone-stop-001'
    out = root / 'builds/milestone-stop-x86-001'
    out.mkdir(parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location('evaluation', source / 'benchmarks/search/eval.py')
    evaluation = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(evaluation)
    identity = evaluation.source_identity(source)
    assert identity['source_tree_sha256'] == args.source_sha256
    features = 'metroid-motion-context,selector-cost-audit'
    command = ['cargo', 'build', '--release', '--locked', '--manifest-path',
               str(source / 'workloads/nes/Cargo.toml'), '--bin', 'nes-eval',
               '--bin', 'metroid-boss-probe', '--features', features,
               '--target-dir', str(root / 'target'), '-j', '8']
    env = {**os.environ, 'HARMONY_SEARCH_SOURCE_SHA256': args.source_sha256}
    with (out / 'build.log').open('w') as log:
        subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT,
                       check=True, timeout=840)
    assert evaluation.source_identity(source) == identity
    for name in ('nes-eval', 'metroid-boss-probe'):
        shutil.copy2(root / 'target/release' / name, out / name)
    metadata = {'format': 'harmony-diagnostic-build-v1', **identity,
                'source_commit': args.source_commit, 'binary': 'nes-eval',
                'binary_sha256': evaluation.digest(out / 'nes-eval'),
                'diagnostic_binary_sha256': evaluation.digest(out / 'metroid-boss-probe'),
                'features': features, 'profile': 'release', 'locked': True,
                'command': command, 'target_cache': str(root / 'target'),
                'cargo': subprocess.check_output(['cargo', '-V'], text=True).strip(),
                'rustc': subprocess.check_output(['rustc', '-Vv'], text=True)}
    (out / 'build-info.json').write_text(json.dumps(metadata, indent=2, sort_keys=True) + '\n')
    print(json.dumps(metadata), flush=True)


if __name__ == '__main__':
    main()
