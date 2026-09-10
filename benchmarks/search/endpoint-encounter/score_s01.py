#!/usr/bin/env python3
"""Check the frozen conditional panel and describe paired observations."""
import argparse
from collections import Counter
import gzip
import hashlib
import json
from pathlib import Path


def raw(root, name):
    path = root / name
    return path.read_bytes() if path.exists() else gzip.decompress(path.with_suffix(path.suffix + '.gz').read_bytes())


def score(protocol, output):
    registration = json.loads((protocol / 's01-registration.json').read_text())
    request = json.loads((protocol / 's01-request.json').read_text())
    summary = json.loads(raw(output, 'summary.json'))
    usage = json.loads(raw(output, 'usage.json'))
    draws = raw(output, 'suffixes.json')
    assert hashlib.sha256(draws).hexdigest() == registration['suffix_sha256']
    suffixes = json.loads(draws)
    assert usage['complete'] and usage['error'] is None
    assert usage['request_sha256'] == hashlib.sha256((protocol / 's01-request.json').read_bytes()).hexdigest()
    rows = summary['trials']
    assert rows == [json.loads(line) for line in raw(output, 'trials.jsonl').splitlines()]
    assert len(rows) == summary['verified_positive_root_restores'] == 64
    assert summary['prefix_physical_frames'] == 118804
    expected_order = [(i, arm) for i in range(32) for arm in
                      (['ordinary', 'passive'] if i % 2 == 0 else ['passive', 'ordinary'])]
    assert [(r['trial'], r['arm']) for r in rows] == expected_order
    used = summary['prefix_physical_frames']
    for row in rows:
        assert row['seed'] == request['trial_seeds'][row['trial']]
        out = row['outcome']
        assert 0 <= out['frames'] <= request['frames_per_arm']
        assert 0 <= out['completed_actions'] <= request['actions']
        if out['first_surviving_damage_frame'] is not None:
            assert 0 < out['first_surviving_damage_frame'] <= out['frames']
            assert out['observed_hp_drop_events'] > 0 and out['observed_hp_loss'] > 0
        if out['surviving_defeat_endpoint']:
            assert not out['dead'] and out['stop_reason'] == 'surviving_defeat'
        used += out['frames']
        assert row['cumulative_physical_frames'] == used
    assert summary['continuation_physical_frames'] == used - summary['prefix_physical_frames']
    prefix = json.loads((protocol / 'first-encounter-input.json').read_text())['actions']
    expected_witnesses = set()
    for arm in ['ordinary', 'passive']:
        for kind, field in [('damage', 'first_surviving_damage_frame'), ('defeat', 'surviving_defeat_endpoint')]:
            candidates = [r for r in rows if r['arm'] == arm and r['outcome'][field]]
            if candidates:
                expected_witnesses.add(f'{arm}-surviving-{kind}.json')
    assert {w['file'] for w in summary['witnesses']} == expected_witnesses
    for item in summary['witnesses']:
        data = raw(output, item['file'])
        assert hashlib.sha256(data).hexdigest() == item['sha256']
        witness = json.loads(data)
        arm = item['file'].split('-')[0]
        field = 'first_surviving_damage_frame' if 'damage' in item['file'] else 'surviving_defeat_endpoint'
        first = next(r for r in rows if r['arm'] == arm and r['outcome'][field])
        selected = suffixes[first['trial']][:witness['completed_actions']]
        if arm == 'passive':
            selected = [{**a, 'buttons': 0} for a in selected]
        assert witness['input']['actions'] == prefix + selected
        assert item['verified_held_replays'] == 2
        assert item['physical_verification_frames'] == 2 * (118804 + item['continuation_frames'])
        used += item['physical_verification_frames']
    assert used == summary['physical_frames'] == usage['physical_frames_known']
    assert used <= registration['auxiliary_frame_ceiling']
    arms = {}
    for arm in ['ordinary', 'passive']:
        outcomes = [r['outcome'] for r in rows if r['arm'] == arm]
        arms[arm] = {
            'trials': len(outcomes),
            'trials_with_observed_hp_drop': sum(o['observed_hp_drop_events'] > 0 for o in outcomes),
            'trials_with_surviving_damage_endpoint': sum(o['first_surviving_damage_frame'] is not None for o in outcomes),
            'surviving_defeat_endpoints': sum(o['surviving_defeat_endpoint'] for o in outcomes),
            'observed_defeat_flags': sum(o['observed_defeat_flag'] for o in outcomes),
            'stops': dict(Counter(o['stop_reason'] for o in outcomes)),
            'physical_frames': sum(o['frames'] for o in outcomes),
        }
    pairs = Counter()
    for i in range(32):
        a, b = (next(r['outcome'] for r in rows if r['trial'] == i and r['arm'] == arm)
                for arm in ['ordinary', 'passive'])
        key = {(False, False): 'neither', (True, False): 'ordinary_only',
               (False, True): 'passive_only', (True, True): 'both'}[
                   (a['first_surviving_damage_frame'] is not None,
                    b['first_surviving_damage_frame'] is not None)]
        pairs[key] += 1
    return {'format': 'metroid-conditional-control-analysis-v1', 'decision': 'complete',
            'registration_sha256': hashlib.sha256((protocol / 's01-registration.json').read_bytes()).hexdigest(),
            'summary_sha256': hashlib.sha256(raw(output, 'summary.json')).hexdigest(),
            'known_auxiliary_frames': used, 'arms': arms, 'paired_surviving_damage': dict(pairs),
            'witnesses': summary['witnesses'],
            'interpretation': 'Conditional observations at one unchanged searched root. Only saved first-per-arm/kind witnesses receive two held replays. No fresh-search improvement, fight-impossibility or global allocation cause is established.'}


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--protocol', type=Path, default=Path(__file__).parent)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    a = p.parse_args()
    a.out.write_text(json.dumps(score(a.protocol, a.output), indent=2) + '\n')
