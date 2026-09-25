#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Build isolated continuation ablations and run a registered local panel."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

from register import source_hash

PATCH_FROM = 'self.continuations = (K::preferences() > 0).then(|| ContinuationBank::new(action_cap));'
PATCH_TO = 'self.continuations = None;\n        let _ = action_cap;'


def prepare(root, out, manifest):
    out.mkdir(parents=True, exist_ok=False)
    if source_hash(root / 'dissonance/searcher', True) != manifest['engine_source_sha256']:
        raise ValueError('engine source differs from registration')
    if source_hash(root / 'workloads/tiny-worlds') != manifest['workload_source_sha256']:
        raise ValueError('workload source differs from registration')
    identities = {}
    for arm in ('enabled', 'disabled'):
        tree = out / arm
        for component in ('dissonance/searcher', 'workloads/tiny-worlds'):
            source = root / component
            destination = tree / component
            destination.mkdir(parents=True)
            shutil.copytree(source / 'src', destination / 'src')
            if (source / 'benches').exists():
                shutil.copytree(source / 'benches', destination / 'benches')
            for path in source.iterdir():
                if path.is_file() and (path.suffix == '.rs' or path.name in ('Cargo.toml', 'Cargo.lock')):
                    shutil.copyfile(path, destination / path.name)
        shutil.copyfile(root / 'dissonance/Cargo.toml', tree / 'dissonance/Cargo.toml')
        if arm == 'disabled':
            path = tree / 'dissonance/searcher/src/search/archive.rs'
            text = path.read_text()
            if text.count(PATCH_FROM) != 1:
                raise ValueError('continuation ablation site changed')
            path.write_text(text.replace(PATCH_FROM, PATCH_TO))
        engine = source_hash(tree / 'dissonance/searcher', True)
        workload = source_hash(tree / 'workloads/tiny-worlds')
        env = dict(os.environ, CARGO_TARGET_DIR=str(out / 'target'), CARGO_BUILD_JOBS='1',
                   CARGO_INCREMENTAL='0', CARGO_PROFILE_RELEASE_DEBUG='0')
        subprocess.run(['cargo', 'build', '--offline', '--locked', '--release', '-j', '1',
                        '--manifest-path', str(tree / 'workloads/tiny-worlds/Cargo.toml')],
                       env=env, check=True, timeout=180)
        binary = out / ('tiny-worlds-' + arm)
        shutil.copyfile(out / 'target/release/tiny-worlds', binary)
        binary.chmod(0o755)
        identities[arm] = dict(engine_source_sha256=engine, workload_source_sha256=workload,
                               binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest())
    (out / 'identities.json').write_text(json.dumps(identities, indent=2) + '\n')


def run(out, manifest, split, panel_bytes):
    identities = json.loads((out / 'identities.json').read_text())
    if identities['enabled']['engine_source_sha256'] != manifest['engine_source_sha256']:
        raise ValueError('prepared engine differs from registration')
    results = []
    started = time.monotonic()
    for arm, identity in identities.items():
        tree = out / arm
        if source_hash(tree / 'dissonance/searcher', True) != identity['engine_source_sha256']:
            raise ValueError('prepared engine changed')
        if source_hash(tree / 'workloads/tiny-worlds') != manifest['workload_source_sha256']:
            raise ValueError('prepared workload changed')
        original = (out / 'enabled/dissonance/searcher/src/search/archive.rs').read_text()
        disabled = (out / 'disabled/dissonance/searcher/src/search/archive.rs').read_text()
        if original.count(PATCH_FROM) != 1 or disabled != original.replace(PATCH_FROM, PATCH_TO):
            raise ValueError('ablation differs from registered single-site mutation')
        binary = out / ('tiny-worlds-' + arm)
        if hashlib.sha256(binary.read_bytes()).hexdigest() != identity['binary_sha256']:
            raise ValueError('prepared binary changed')
        for case in manifest[split]:
            for seed in manifest[split + '_seeds']:
                request = dict(config=case['config'], seed=seed, work_budget=manifest['work_budget'],
                               broken=False, verify=True)
                completed = subprocess.run([str(binary)], input=json.dumps(request), text=True,
                                           capture_output=True, check=True, timeout=30)
                result = json.loads(completed.stdout)
                for field in ('engine_source_sha256', 'workload_source_sha256'):
                    if result[field] != identity[field]:
                        raise ValueError('binary source mismatch: ' + field)
                for field in ('config', 'seed', 'work_budget', 'broken'):
                    if result[field] != request[field]:
                        raise ValueError('request mismatch: ' + field)
                if result['verified'] is not True:
                    raise ValueError('unverified run')
                if arm == 'disabled' and result['continuation_jobs'] != 0:
                    raise ValueError('continuations remain enabled in control')
                result.update(case=case['id'], arm=arm)
                results.append(result)
    report = dict(schema=1, split=split, panel_sha256=hashlib.sha256(panel_bytes).hexdigest(),
                  identities=identities, elapsed_seconds=time.monotonic() - started, results=results)
    (out / (split + '.json')).write_text(json.dumps(report, sort_keys=True) + '\n')
    for case in manifest[split]:
        for arm in identities:
            rows = [r for r in results if r['case'] == case['id'] and r['arm'] == arm]
            transfers = [t for r in rows for t in r['route_evidence']['upgraded_continuation_transfers']]
            print(json.dumps(dict(case=case['id'], arm=arm, successes=sum(r['success'] for r in rows),
                trials=len(rows), work=[r['first_objective_work'] for r in rows],
                upgraded_transfers=len(transfers), old_route_transfers=sum(t['pre_upgrade_donor'] for t in transfers))))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--prepare', action='store_true')
    parser.add_argument('--split', choices=('development', 'validation'))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    panel_bytes = Path(__file__).with_name('route_reuse.json').read_bytes()
    manifest = json.loads(panel_bytes)
    out = args.out.resolve()
    if args.prepare:
        prepare(root, out, manifest)
    if args.split:
        run(out, manifest, args.split, panel_bytes)
    if not args.prepare and not args.split:
        parser.error('choose --prepare and/or --split')


if __name__ == '__main__':
    main()
