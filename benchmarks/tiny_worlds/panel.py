#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run the registered tiny-world comparisons; use supervise.py for admission."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import subprocess
import time

from route_reuse import run_ablation


def interval(successes, count):
    z = 1.96
    center = (successes / count + z * z / (2 * count)) / (1 + z * z / count)
    radius = z * math.sqrt(successes / count * (1 - successes / count) / count + z * z / (4 * count * count)) / (1 + z * z / count)
    return [max(0, center - radius), min(1, center + radius)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--route-reuse', action='store_true', help='run the registered continuation-bank ablation')
    parser.add_argument('--out', type=Path, help='prepared ablation directory outside the repository')
    parser.add_argument('--prepare', action='store_true', help='build both ablation arms in a fresh output directory')
    parser.add_argument('--panel', type=Path, default=Path(__file__).with_name('panel.json'))
    parser.add_argument('--split', choices=('development', 'validation', 'sweep', 'scaling', 'confirmation'), default='development')
    args = parser.parse_args()
    if args.route_reuse:
        if args.out is None or args.split not in ('development', 'validation') or args.binary is not None or args.panel != Path(__file__).with_name('panel.json'):
            parser.error('--route-reuse requires --out and a development/validation split, without --binary/--panel')
        run_ablation(args.out, args.split, args.prepare)
        return
    if args.binary is None or args.out is not None or args.prepare:
        parser.error('ordinary panels require --binary; --out/--prepare require --route-reuse')
    raw = args.panel.read_bytes()
    manifest = json.loads(raw)
    if manifest['schema'] != 2:
        raise ValueError('unsupported panel schema')
    cases = manifest[args.split] if args.split != 'sweep' else [
        dict(id=case['id'] + '-work-' + str(budget), config=case['config'], work_budget=budget)
        for case in manifest['sweep_cases'] for budget in manifest['sweep_budgets']]
    if not cases:
        raise ValueError("selected split has no registered cases")
    results = []
    started = time.monotonic()
    for case in cases:
        for broken in manifest['arms']:
            for seed in manifest[args.split + '_seeds']:
                request = dict(config=case['config'], seed=seed, broken=broken,
                               work_budget=case.get('work_budget', manifest['work_budget']), verify=True)
                completed = subprocess.run([str(args.binary.resolve())], input=json.dumps(request),
                                           text=True, capture_output=True, timeout=30, check=True)
                result = json.loads(completed.stdout)
                if result['engine_source_sha256'] != manifest['engine_source_sha256']:
                    raise ValueError('binary engine source does not match registered baseline')
                if result['workload_source_sha256'] != manifest['workload_source_sha256']:
                    raise ValueError('binary workload source does not match registered workload')
                for field in ('config', 'seed', 'broken', 'work_budget'):
                    if result[field] != request[field]:
                        raise ValueError('binary request echo mismatch: ' + field)
                if result['verified'] is not True:
                    raise ValueError('binary did not verify exact invariants')
                result['case'] = case['id']
                results.append(result)
    comparisons = []
    for case in cases:
        for arm in manifest['arms']:
            rows = [r for r in results if r['case'] == case['id'] and r['broken'] == arm]
            successes = sum(r['success'] for r in rows)
            comparisons.append(dict(case=case['id'], broken=arm, successes=successes,
                                    trials=len(rows), wilson_95=interval(successes, len(rows)),
                                    work_to_objective=[dict(seed=r['seed'], event=r['success'],
                                        work=r['first_objective_work'] if r['success'] else r['work_budget'])
                                        for r in rows]))
    print(json.dumps(dict(schema=2, split=args.split, panel_sha256=hashlib.sha256(raw).hexdigest(),
                          engine_baseline=manifest['baseline'],
                          binary_sha256=hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                          elapsed_seconds=time.monotonic() - started,
                          comparisons=comparisons, results=results), sort_keys=True))


if __name__ == '__main__':
    main()
