#!/usr/bin/env python3
"""Build the isolated experimental evaluation binary from its committed source archive."""
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
    parser.add_argument('--archive-sha256', required=True)
    parser.add_argument('--source-commit', required=True)
    args = parser.parse_args()
    root = args.root.resolve()
    source = root / 'source-action-correlation-001'
    spec = importlib.util.spec_from_file_location('evaluation', source / 'benchmarks/search/eval.py')
    evaluation = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(evaluation)
    assert evaluation.digest(root / 'harmony-action-correlation-001.tar') == args.archive_sha256
    identity = evaluation.source_identity(source)
    out = root / 'builds/action-correlation-001'
    out.mkdir(parents=True, exist_ok=False)
    env = {**os.environ, 'PATH': '/root/.cargo/bin:' + os.environ['PATH'],
           'RUSTUP_TOOLCHAIN': '1.97.1', 'HARMONY_SEARCH_SOURCE_SHA256': identity['source_tree_sha256']}
    command = ['cargo', 'build', '--release', '--locked', '--manifest-path',
               str(source / 'workloads/nes/Cargo.toml'), '--bin', 'nes-eval',
               '--features', 'metroid-motion-context,selector-cost-audit',
               '--target-dir', str(root / 'target'), '-j', '4']
    with (out / 'build.log').open('w') as log:
        subprocess.run(command, env=env, cwd=source, stdout=log, stderr=subprocess.STDOUT,
                       timeout=600, check=True)
    assert evaluation.source_identity(source) == identity
    shutil.copy2(root / 'target/release/nes-eval', out / 'nes-eval')
    metadata = {'format': 'harmony-search-build-v1', **identity,
                'source_commit': args.source_commit,
                'source_archive_sha256': args.archive_sha256,
                'binary': 'nes-eval',
                'binary_sha256': evaluation.digest(out / 'nes-eval'),
                'features': 'metroid-motion-context,selector-cost-audit',
                'profile': 'release', 'locked': True, 'command': command,
                'cargo': subprocess.check_output(['cargo', '-V'], env=env, text=True).strip(),
                'rustc': subprocess.check_output(['rustc', '-Vv'], env=env, text=True)}
    (out / 'build-info.json').write_text(json.dumps(metadata, indent=2, sort_keys=True) + '\n')
    print(json.dumps(metadata), flush=True)


if __name__ == '__main__':
    main()
