#!/usr/bin/env python3
"""Verify native observer compatibility and restore evidence, without emulation."""
import argparse
from copy import deepcopy
import hashlib
import json
from pathlib import Path

from boss_interval import BossIntervalObserver


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read(path):
    return json.loads(path.read_text())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    base = args.root / 'depth-transfer-20260909'
    reg = read(base / 'nq02-registration.json')
    cases = {case['id']: read(base / case['id'] / 'summary.json') for case in reg['cases']}
    for mode in ('default', 'area'):
        for name in ('summary.json', 'relevant-frames.jsonl'):
            assert (base / ('nq02-' + mode) / name).read_bytes() == (base / ('nq01-' + mode) / name).read_bytes()
    area = cases['nq02-area']
    area_trace = (base / 'nq02-area/relevant-frames.jsonl').read_bytes()
    checked_intervals = 0
    for mode, restores in (('context', 0), ('restores', 31)):
        summary = cases['nq02-' + mode]
        normalized = deepcopy(summary)
        assert normalized.pop('restore_qualification_period_frames') == (4096 if restores else None)
        assert normalized['format'] == 'metroid-boss-memory-probe-v3'
        normalized['format'] = area['format']
        assert normalized['trace_observation_policy'] == 'boss_context_intervals_v1'
        normalized['trace_observation_policy'] = area['trace_observation_policy']
        assert normalized['columns'][-2:] == ['saved_status_by_slot', 'interval_by_slot']
        normalized['columns'] = normalized['columns'][:-2]
        normalized['limitations'] = normalized['limitations'][:-2]
        diagnostics = normalized['one_frame'].pop('context_diagnostics')
        assert diagnostics['verified_self_restores'] == restores
        normalized['one_frame']['relevant_trace_bytes'] = area['one_frame']['relevant_trace_bytes']
        assert normalized == area
        trace = base / ('nq02-' + mode) / 'relevant-frames.jsonl'
        assert trace.stat().st_size == summary['one_frame']['relevant_trace_bytes'] <= reg['trace_limit_bytes']
        rows = [json.loads(line) for line in trace.read_text().splitlines()]
        projected = b''.join((json.dumps(row[:9], separators=(',', ':')) + '\n').encode() for row in rows)
        assert projected == area_trace
        observer, previous = BossIntervalObserver(), None
        for row in rows:
            assert len(row) == 11 and len(row[8]) == len(row[9]) == len(row[10]) == 6
            raw = dict(frame=row[0], area=row[1], mode=row[2])
            for slot, fields in enumerate(row[8]):
                for name, value in zip(('offset', 'status', 'type', 'special', 'hp', 'x', 'y', 'name_table'), fields):
                    raw[f'slot{slot}_{name}'] = value
                raw[f'slot{slot}_saved_status'] = row[9][slot]
            if previous != row[0] - 1:
                observer.reset()
            report = observer.observe(0, row[0], raw)
            if previous == row[0] - 1:
                expected = [None] * 6
                for event in report['intervals']:
                    expected[event['slot']] = {key: event[key] for key in ('kind', 'hp_loss')}
                assert expected == row[10]
                checked_intervals += 1
            previous = row[0]
    context, restored = deepcopy(cases['nq02-context']), deepcopy(cases['nq02-restores'])
    restored['restore_qualification_period_frames'] = context['restore_qualification_period_frames']
    restored['one_frame']['context_diagnostics']['verified_self_restores'] = 0
    assert restored == context
    assert (base / 'nq02-context/relevant-frames.jsonl').read_bytes() == (base / 'nq02-restores/relevant-frames.jsonl').read_bytes()
    charge, evidence = 0, []
    for case in reg['cases']:
        path = base / case['id'] / 'summary.json'
        summary = cases[case['id']]
        assert summary['verified_replays'] == 3
        for key in ('input_sha256', 'rom_sha256', 'core_sha256'):
            assert summary[key] == reg[key]
        actual = summary['ordinary']['physical_frames_including_setup'] + 2 * summary['one_frame']['physical_frames_including_setup']
        assert actual == reg['expected_physical_frames_per_case']
        charge += actual
        evidence.append({'id':case['id'], 'summary_sha256':sha(path),
                         'trace_sha256':sha(path.with_name('relevant-frames.jsonl')),
                         'known_physical_frames':actual, 'summary':summary})
    assert charge == reg['expected_total_physical_frames'] <= reg['auxiliary_frame_ceiling']
    result = {'format':'depth-transfer-nq02-results-v1', 'decision':'pass',
              'registration_sha256':sha(base / 'nq02-registration.json'),
              'checker_sha256':sha(Path(__file__)), 'cases':evidence,
              'known_auxiliary_frames':charge, 'python_checked_retained_intervals':checked_intervals,
              'verified_self_restores':62,
              'interpretation':'One reused searched route; positive classification is separately checked on public-reference bytes. Not a fresh search result or universal restore/slot-lifetime proof.'}
    assert not args.out.exists()
    args.out.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({key:result[key] for key in ('decision','known_auxiliary_frames','python_checked_retained_intervals','verified_self_restores')}))


if __name__ == '__main__':
    main()
