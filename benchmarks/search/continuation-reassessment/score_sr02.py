#!/usr/bin/env python3
"""Prospective SR02 efficacy/futility gates; exact interval and ratio rules."""
import json
from pathlib import Path
from score_sr01 import assess as qualified_assess

ARMS = {'ordinary-control', 'progress-control', 'progress-half', 'capacity-half'}
CONTROLS = ['progress-control', 'ordinary-control', 'capacity-half']


def milestone_interval(q, result, stream):
    """Restricted first-observation interval, including search constructors.

    The event is observed at ordered job admission. Its precise position inside
    that job is unknown; no cached-state gap or success-only average estimates it.
    Already reserved drain is reported/spent work, not earlier event discovery.
    """
    setup = (q['workers'] + 1) * 929
    budget = q['frames'] + setup
    if not result['milestone_reached_within_budget']:
        return [budget, budget]
    event = result['first_milestone']
    total = 0
    with Path(stream).open() as lines:
        header = json.loads(next(lines))
        assert header['origin_kind'] == 'snapshot_root' and header['resume_actions'] == 0
        for line in lines:
            row = json.loads(line)
            if row.get('event') != 'job':
                continue
            previous = total
            total += row['frames']
            if row['sequence'] == event['execution']:
                assert row['frames'] > 0 and total == event['frames_emulated'] <= q['frames']
                return [previous + setup, total + setup]
    raise AssertionError('First milestone has no matching admitted job')


def assess(master, cell, out):
    row = qualified_assess(master, cell, out)
    q = json.loads((Path(master['protocol']) / cell['request']).read_text())
    result = json.loads((out / 'result.json').read_text())
    assert q['frames'] == 1_000_000
    row['restricted_interval'] = milestone_interval(q, result, out / 'stream.jsonl')
    return row


def panel_score(rows, pairs=4):
    if any(r.get('valid') is not True for r in rows):
        return dict(status='invalid', reason='A cell failed its frozen validity gate.')
    grouped = {}
    for row in rows:
        arms = grouped.setdefault(row['pair'], {})
        assert row['arm'] not in arms, 'Duplicate cell'
        arms[row['arm']] = row
        low, high = row['restricted_interval']
        assert 0 <= low <= high <= 1_004_645
    assert len(grouped) <= pairs and sorted(grouped) == list(range(len(grouped)))
    assert all(set(arms) == ARMS for arms in grouped.values())
    assert all(len({r['seed'] for r in arms.values()}) == 1 for arms in grouped.values())
    assert len({arms['progress-half']['seed'] for arms in grouped.values()}) == len(grouped)
    remaining = pairs - len(grouped)
    comparisons = {}
    for comparator in CONTROLS:
        candidate = [arms['progress-half'] for arms in grouped.values()]
        controls = [arms[comparator] for arms in grouped.values()]
        wins = sum(a['restricted_interval'][1] < b['restricted_interval'][0]
                   for a, b in zip(candidate, controls))
        upper = sum(a['restricted_interval'][1] for a in candidate)
        lower = sum(b['restricted_interval'][0] for b in controls)
        cpu_candidate = sum(a['cpu_microseconds'] for a in candidate)
        cpu_control = sum(b['cpu_microseconds'] for b in controls)
        memory_candidate = sum(a['peak_rss_kib'] for a in candidate)
        memory_control = sum(b['peak_rss_kib'] for b in controls)
        comparisons[comparator] = dict(
            strict_interval_wins=wins, completed_pairs=len(grouped),
            candidate_upper_sum=upper, comparator_lower_sum=lower,
            worst_case_mean_ratio=upper / lower if lower else None,
            ratio_pass=bool(lower) and 100 * upper <= 85 * lower,
            candidate_cpu_microseconds=cpu_candidate, comparator_cpu_microseconds=cpu_control,
            cpu_ratio=cpu_candidate / cpu_control if cpu_control else None,
            candidate_peak_rss_kib_sum=memory_candidate, comparator_peak_rss_kib_sum=memory_control,
            memory_ratio=memory_candidate / memory_control if memory_control else None,
            resources_pass=(cpu_control > 0 and memory_control > 0
                            and 100 * cpu_candidate <= 125 * cpu_control
                            and 100 * memory_candidate <= 125 * memory_control))
    futile = any(c['strict_interval_wins'] + remaining < 3 for c in comparisons.values())
    passed = remaining == 0 and all(c['strict_interval_wins'] >= 3 and c['ratio_pass']
                                   and c['resources_pass'] for c in comparisons.values())
    return dict(status='passed' if passed else 'failed' if remaining == 0 else 'futile' if futile else 'continue',
                comparisons=comparisons, unrun_pairs=remaining,
                scope='Conditional supplied-root endpoint screen only; no fresh-search or power claim.')
