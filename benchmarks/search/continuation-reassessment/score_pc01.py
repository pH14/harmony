#!/usr/bin/env python3
"""Frozen finite-bank comparison. No independence, efficacy or HP-only claim."""
import json
from pathlib import Path
import sys


def resources_at_least(a, b):
    return (all(a[k] >= b[k] for k in ('health', 'missiles', 'missile_capacity', 'energy_tanks'))
            and all(a[k] & b[k] == b[k] for k in ('equipment', 'bosses')))


def new_progress_advantage(a, b):
    return (a['complete'] and b['complete'] and not a['dead'] and not b['dead']
            and a['frames'] == b['frames'] and a['action'] == b['action']
            and a['root_interval_valid'] and b['root_interval_valid']
            and a['new_hp_loss'] is not None and b['new_hp_loss'] is not None
            and a['hp'] is not None and b['hp'] is not None
            and a['new_hp_loss'] > b['new_hp_loss'] and a['hp'] < b['hp']
            and resources_at_least(a['state'], b['state']))


def surviving_defeat(outcome):
    return any(p['complete'] and not p['dead'] and p['defeat'] for p in outcome['points'])


def score(rows, bank):
    assert rows[0]['kind'] == 'qualified_roots'
    trials = rows[1:]
    assert len(trials) == 2 * len(bank['seeds'])
    expected_order = [(i, arm) for i in range(len(bank['seeds']))
                      for arm in (['candidate', 'incumbent'] if i % 2 == 0
                                  else ['incumbent', 'candidate'])]
    assert [(r['trial'], r['arm']) for r in trials] == expected_order
    indexed = {(r['trial'], r['arm']): r['outcome'] for r in trials}
    for r in trials:
        assert r['seed'] == bank['seeds'][r['trial']]
        o = r['outcome']
        assert o['held_endpoint_verified']
        assert o['held_boundaries_verified'] == len(o['points'])
        assert all(p['action'] == i + 1 for i, p in enumerate(o['points']))
        assert all(not p['root_interval_valid'] or
                   p['new_hp_loss'] == o['root_hp'] - p['hp'] for p in o['points'])
    pairs = []
    for i, seed in enumerate(bank['seeds']):
        a, b = [indexed[i, arm] for arm in ('candidate', 'incumbent')]
        common = [(x, y) for x, y in zip(a['points'], b['points'])
                  if x['complete'] and y['complete'] and x['frames'] == y['frames']]
        first = lambda side: next(({'action':x['action'], 'frames':x['frames'],
                                   'candidate':x,'incumbent':y} for x, y in common
                                   if new_progress_advantage(*((x,y) if side == 0 else (y,x)))), None)
        pairs.append(dict(trial=i, seed=seed, candidate_first_advantage=first(0),
                          incumbent_first_advantage=first(1),
                          candidate_surviving_defeat=surviving_defeat(a),
                          incumbent_surviving_defeat=surviving_defeat(b),
                          common_completed_boundaries=len(common),
                          common_valid_boundaries=sum(x['root_interval_valid'] and
                                                      y['root_interval_valid'] for x,y in common),
                          candidate_stop=a['stop_reason'], incumbent_stop=b['stop_reason'],
                          candidate_frames=a['frames'], incumbent_frames=b['frames']))
    ca = sum(p['candidate_first_advantage'] is not None for p in pairs)
    ia = sum(p['incumbent_first_advantage'] is not None for p in pairs)
    cd = sum(p['candidate_surviving_defeat'] for p in pairs)
    ind = sum(p['incumbent_surviving_defeat'] for p in pairs)
    defeat_advantage = any(p['candidate_surviving_defeat'] and not p['incumbent_surviving_defeat'] for p in pairs)
    return dict(format='retention-pc01-analysis-v1', pairs=pairs,
                candidate_advantage_tails=ca, incumbent_advantage_tails=ia,
                candidate_surviving_defeats=cd, incumbent_surviving_defeats=ind,
                continuation_frames=sum(r['outcome']['frames'] for r in trials),
                decision=('paired_continuation_counterexample_found' if ca or defeat_advantage
                          else 'no_candidate_counterexample_in_fixed_bank'),
                scope='One selected state pair and a fixed finite suffix bank. Both directions and unavailable intervals retained. No population, HP-only, retention-policy or fresh-search claim.')


if __name__ == '__main__':
    output, bank = map(Path, sys.argv[1:])
    rows = [json.loads(line) for line in (output / 'trials.jsonl').read_text().splitlines()]
    print(json.dumps(score(rows, json.loads(bank.read_text())), indent=2))
