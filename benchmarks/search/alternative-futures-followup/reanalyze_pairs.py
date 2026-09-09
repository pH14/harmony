#!/usr/bin/env python3
"""F01: descriptive reanalysis of frozen P03; no emulator or controller export."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import resource
import time

EXPECTED = {
    'audit': 'cae156018d08bbe61364442f936224528bbf7a151b7319a2209cc8c6a0d48b46',
    'outcomes': 'c79df7524bbb28d0dc3ebc8a86f7028b60380469f5df03a29f4b66cd137bc945',
    'suffixes': '4e408f2397801eb5a3946a6b7e5ebfda8954669b87f87ec65478e7a7cb94646d',
}
MAX_BYTES = 256 * 1024 * 1024


def bounded(path):
    with path.open('rb') as f:
        data = f.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        raise ValueError('input exceeds preregistered bound')
    return data


def analyze(audit, rows):
    pairs = audit['samples'][2]
    if len(pairs) != 16 or any(audit['samples'][i] for i in [0, 1, 3, 4]):
        raise ValueError('expected the frozen 16 resource-tradeoff pairs only')
    grouped = [[] for _ in pairs]
    for row in rows:
        i = row['pair']
        if row['stratum'] != 2 or not 0 <= i < 16:
            raise ValueError('unexpected pair identity')
        p = pairs[i]
        if row['execution'] != p['execution'] or row['candidate_replaces'] != p['replaces']:
            raise ValueError('source identity mismatch')
        grouped[i].append(row)
    result = []
    for i, (p, rs) in enumerate(zip(pairs, grouped)):
        if len(rs) != 64 or {v['trial'] for v in rs} != set(range(64)):
            raise ValueError('missing or repeated suffix trial')
        d, s = (p['incumbent'], p['candidate']) if p['replaces'] else (p['candidate'], p['incumbent'])
        features = {}
        for label, state in [('discarded', d), ('survivor', s)]:
            features[label] = state
        features['source_same_map'] = all(d[k] == s[k] for k in ['area', 'map_x', 'map_y'])
        features['source_same_capabilities'] = all(d[k] == s[k] for k in ['equipment', 'bosses', 'energy_tanks', 'missile_capacity'])
        features['source_same_position'] = (d['x'], d['y']) == (s['x'], s['y'])
        features['source_same_pose'] = d['pose'] == s['pose']
        features['candidate_actions'] = len(p['candidate_input']['actions'])
        features['incumbent_actions'] = len(p['incumbent_input']['actions'])
        counts = Counter()
        for row in rs:
            do, so = row['discarded'], row['survivor']
            dm, sm = set(map(tuple, do['reached_maps'])), set(map(tuple, so['reached_maps']))
            dx, sx = bool(dm - sm), bool(sm - dm)
            if dx != row['discarded_only_exit'] or sx != row['survivor_only_exit']:
                raise ValueError('reported exit does not match map set difference')
            if row['discarded_only_gain'] or row['survivor_only_gain']:
                raise ValueError('unexpected gain in frozen P03')
            counts['trials'] += 1
            counts['discarded_frames'] += do['frames']
            counts['survivor_frames'] += so['frames']
            counts['discarded_only_exit'] += dx
            counts['survivor_only_exit'] += sx
            counts[f"survival_D{int(not do['dead'])}_S{int(not so['dead'])}"] += 1
            for label, include in [('both_alive', not do['dead'] and not so['dead']), ('equal_actual_frames', do['frames'] == so['frames'])]:
                if include:
                    counts[label + '_trials'] += 1
                    counts[label + '_discarded_only_exit'] += dx
                    counts[label + '_survivor_only_exit'] += sx
        result.append({'pair': i, 'execution': p['execution'], 'features': features, 'counts': dict(counts)})
    totals = Counter()
    for r in result:
        totals.update(r['counts'])
    for key, expected in [('trials', 1024), ('discarded_frames', 707878), ('survivor_frames', 489897), ('discarded_only_exit', 156), ('survivor_only_exit', 42), ('survival_D1_S0', 197)]:
        if totals[key] != expected:
            raise ValueError('frozen original aggregate mismatch: ' + key)
    signs = {}
    for label, dk, sk in [('all', 'discarded_only_exit', 'survivor_only_exit'), ('both_alive', 'both_alive_discarded_only_exit', 'both_alive_survivor_only_exit'), ('equal_actual_frames', 'equal_actual_frames_discarded_only_exit', 'equal_actual_frames_survivor_only_exit')]:
        signs[label] = dict(Counter('discarded_more' if r['counts'].get(dk, 0) > r['counts'].get(sk, 0) else 'survivor_more' if r['counts'].get(dk, 0) < r['counts'].get(sk, 0) else 'tie' for r in result))
    return {'pairs': result, 'totals': dict(totals), 'pair_signs': signs,
            'limitations': ['One development campaign, reservoir pairs conditional on cached snapshot and bounded source input availability.', 'Both-alive and equal-frame subsets condition on outcomes; descriptive, not a causal matched-work intervention.', 'Trial exit flags can favor both sides on different maps.', 'No timestamps or archive membership in frozen outcomes; global novelty unknown.']}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('audit', type=Path)
    p.add_argument('outcomes', type=Path)
    p.add_argument('suffixes', type=Path)
    p.add_argument('output', type=Path)
    a = p.parse_args()
    start = time.monotonic()
    data, hashes = {}, {}
    for key in EXPECTED:
        data[key] = bounded(getattr(a, key))
        hashes[key] = hashlib.sha256(data[key]).hexdigest()
        if hashes[key] != EXPECTED[key]:
            raise ValueError('unexpected frozen hash: ' + key)
    result = analyze(json.loads(data['audit']), (json.loads(line) for line in data['outcomes'].splitlines()))
    result.update(format='alternative-futures-F01-v1', source_sha256=hashes, new_emulator_frames=0, elapsed_seconds=time.monotonic() - start, cpu_seconds=resource.getrusage(resource.RUSAGE_SELF).ru_utime + resource.getrusage(resource.RUSAGE_SELF).ru_stime)
    with a.output.open('x') as f:
        json.dump(result, f, indent=2)
        f.write('\n')
    print(json.dumps({'totals': result['totals'], 'pair_signs': result['pair_signs']}))


if __name__ == '__main__':
    main()
