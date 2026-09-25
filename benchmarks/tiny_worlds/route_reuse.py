# SPDX-License-Identifier: AGPL-3.0-or-later
"""Continuation ablation support for the public panel command."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

from register import source_hash

CONTROL = 'TINY_WORLD_DISABLE_CONTINUATIONS'
PATCH_FROM = 'self.continuations = (K::preferences() > 0).then(|| ContinuationBank::new(action_cap));'
PATCH_TO = ('self.continuations = (std::env::var_os("' + CONTROL + '").is_none() && K::preferences() > 0)'
            '.then(|| ContinuationBank::new(action_cap));')
ARCHIVE = Path('searcher/src/search/archive.rs')


def prepare(root, out, manifest):
    out.mkdir(parents=True, exist_ok=False)
    if source_hash(root / 'dissonance/searcher', True) != manifest['engine_source_sha256']:
        raise ValueError('engine source differs from registration')
    if source_hash(root / 'workloads/tiny-worlds') != manifest['workload_source_sha256']:
        raise ValueError('workload source differs from registration')
    tree = out / 'source'
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
    shutil.copytree(tree / 'dissonance', out / 'baseline/dissonance')
    path = tree / 'dissonance' / ARCHIVE
    text = path.read_text()
    if text.count(PATCH_FROM) != 1:
        raise ValueError('continuation ablation site changed')
    path.write_text(text.replace(PATCH_FROM, PATCH_TO))
    identity = dict(baseline_engine_source_sha256=manifest['engine_source_sha256'],
                    engine_source_sha256=source_hash(tree / 'dissonance/searcher', True),
                    workload_source_sha256=source_hash(tree / 'workloads/tiny-worlds'))
    env = dict(os.environ, CARGO_TARGET_DIR=str(out / 'target'), CARGO_BUILD_JOBS='1',
               CARGO_INCREMENTAL='0', CARGO_PROFILE_RELEASE_DEBUG='0')
    subprocess.run(['cargo', 'build', '--offline', '--locked', '--release', '-j', '1',
                    '--manifest-path', str(tree / 'workloads/tiny-worlds/Cargo.toml')],
                   env=env, check=True, timeout=180)
    binary = out / 'tiny-worlds'
    shutil.copyfile(out / 'target/release/tiny-worlds', binary)
    binary.chmod(0o755)
    identity['binary_sha256'] = hashlib.sha256(binary.read_bytes()).hexdigest()
    (out / 'identity.json').write_text(json.dumps(identity, indent=2) + '\n')


def run(out, manifest, split, panel_bytes):
    identity = json.loads((out / 'identity.json').read_text())
    tree = out / 'source'
    baseline = out / 'baseline/dissonance'
    if source_hash(baseline / 'searcher', True) != manifest['engine_source_sha256']:
        raise ValueError('prepared baseline differs from registration')
    if source_hash(tree / 'dissonance/searcher', True) != identity['engine_source_sha256']:
        raise ValueError('prepared diagnostic engine changed')
    if source_hash(tree / 'workloads/tiny-worlds') != manifest['workload_source_sha256']:
        raise ValueError('prepared workload changed')
    originals = {p.relative_to(baseline): p.read_bytes() for p in baseline.rglob('*') if p.is_file()}
    actual = {p.relative_to(tree / 'dissonance'): p.read_bytes()
              for p in (tree / 'dissonance').rglob('*') if p.is_file()}
    if originals[ARCHIVE].count(PATCH_FROM.encode()) != 1:
        raise ValueError('ablation source site changed')
    originals[ARCHIVE] = originals[ARCHIVE].replace(PATCH_FROM.encode(), PATCH_TO.encode())
    if originals != actual:
        raise ValueError('diagnostic engine differs beyond the single-site instrumentation')
    binary = out / 'tiny-worlds'
    if hashlib.sha256(binary.read_bytes()).hexdigest() != identity['binary_sha256']:
        raise ValueError('prepared binary changed')
    results = []
    started = time.monotonic()
    for arm in ('enabled', 'disabled'):
        env = dict(os.environ)
        env.pop(CONTROL, None)
        if arm == 'disabled':
            env[CONTROL] = '1'
        for case in manifest[split]:
            for seed in manifest[split + '_seeds']:
                request = dict(config=case['config'], seed=seed, work_budget=manifest['work_budget'],
                               broken=False, verify=True)
                completed = subprocess.run([str(binary)], input=json.dumps(request), text=True,
                                           env=env, capture_output=True, check=True, timeout=30)
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
                result.update(case=case['id'], arm=arm, binary_sha256=identity['binary_sha256'],
                              control_environment={CONTROL: env.get(CONTROL)})
                results.append(result)
    report = dict(schema=2, split=split, panel_sha256=hashlib.sha256(panel_bytes).hexdigest(),
                  identity=identity, elapsed_seconds=time.monotonic() - started, results=results)
    (out / (split + '.json')).write_text(json.dumps(report, sort_keys=True) + '\n')
    for case in manifest[split]:
        for arm in ('enabled', 'disabled'):
            rows = [r for r in results if r['case'] == case['id'] and r['arm'] == arm]
            transfers = [t for r in rows for t in r['route_evidence']['upgraded_continuation_transfers']]
            print(json.dumps(dict(case=case['id'], arm=arm, successes=sum(r['success'] for r in rows),
                trials=len(rows), work=[r['first_objective_work'] for r in rows],
                upgraded_transfers=len(transfers), non_upgraded_donor_transfers=sum(t['non_upgraded_donor'] for t in transfers))))


def run_ablation(out, split, prepare_sources=False):
    root = Path(__file__).resolve().parents[2]
    panel_bytes = Path(__file__).with_name('route_reuse.json').read_bytes()
    manifest = json.loads(panel_bytes)
    if manifest['schema'] != 2:
        raise ValueError('unsupported route panel schema')
    out = out.resolve()
    if prepare_sources:
        prepare(root, out, manifest)
    run(out, manifest, split, panel_bytes)
