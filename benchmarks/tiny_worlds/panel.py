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


def interval(successes, count):
    z = 1.96
    center = (successes / count + z * z / (2 * count)) / (1 + z * z / count)
    radius = z * math.sqrt(successes / count * (1 - successes / count) / count + z * z / (4 * count * count)) / (1 + z * z / count)
    return [max(0, center - radius), min(1, center + radius)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--panel', type=Path, default=Path(__file__).with_name('panel.json'))
    parser.add_argument('--split', choices=('development', 'validation'), default='development')
    args = parser.parse_args()
    raw = args.panel.read_bytes()
    manifest = json.loads(raw)
    if manifest['schema'] != 1:
        raise ValueError('unsupported panel schema')
    results = []
    started = time.monotonic()
    for case in manifest[args.split]:
        for omit_stock in manifest['arms']:
            for seed in manifest[args.split + '_seeds']:
                request = dict(config=case['config'], seed=seed, omit_stock=omit_stock,
                               work_budget=manifest['work_budget'], verify=True)
                completed = subprocess.run([str(args.binary.resolve())], input=json.dumps(request),
                                           text=True, capture_output=True, timeout=30, check=True)
                result = json.loads(completed.stdout)
                if result['engine_source_sha256'] != manifest['engine_source_sha256']:
                    raise ValueError('binary engine source does not match registered baseline')
                if result['workload_source_sha256'] != manifest['workload_source_sha256']:
                    raise ValueError('binary workload source does not match registered workload')
                for field in ('config', 'seed', 'omit_stock', 'work_budget'):
                    if result[field] != request[field]:
                        raise ValueError('binary request echo mismatch: ' + field)
                if result['verified'] is not True:
                    raise ValueError('binary did not verify exact invariants')
                result['case'] = case['id']
                results.append(result)
    comparisons = []
    for case in manifest[args.split]:
        for arm in manifest['arms']:
            rows = [r for r in results if r['case'] == case['id'] and r['omit_stock'] == arm]
            successes = sum(r['success'] for r in rows)
            comparisons.append(dict(case=case['id'], omit_stock=arm, successes=successes,
                                    trials=len(rows), wilson_95=interval(successes, len(rows)),
                                    work_to_objective=[dict(seed=r['seed'], event=r['success'],
                                        work=r['first_objective_work'] if r['success'] else r['work_budget'])
                                        for r in rows]))
    print(json.dumps(dict(schema=1, split=args.split, panel_sha256=hashlib.sha256(raw).hexdigest(),
                          engine_baseline=manifest['baseline'],
                          binary_sha256=hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                          elapsed_seconds=time.monotonic() - started,
                          comparisons=comparisons, results=results), sort_keys=True))


if __name__ == '__main__':
    main()
