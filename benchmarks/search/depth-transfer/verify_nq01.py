#!/usr/bin/env python3
"""Verify native trace-mode compatibility from the completed replay artifacts."""
import argparse
from copy import deepcopy
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read(path):
    return json.loads(path.read_text())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    root = args.root
    base = root / 'depth-transfer-20260909'
    registration = read(base / 'nq01-registration.json')
    old_path = root / registration['legacy_summary']
    old = read(old_path)
    assert sha(old_path) == registration['legacy_summary_sha256']
    default_path = base / 'nq01-default/summary.json'
    assert default_path.read_bytes() == old_path.read_bytes()
    assert (base / 'nq01-default/relevant-frames.jsonl').read_bytes() == old_path.with_name('relevant-frames.jsonl').read_bytes()
    area_path = base / 'nq01-area/summary.json'
    area = read(area_path)
    assert area['format'] == 'metroid-boss-memory-probe-v2'
    assert area['trace_observation_policy'] == 'boss_area_all_frames_v1'
    normalized = deepcopy(area)
    normalized['format'] = old['format']
    normalized.pop('trace_observation_policy')
    normalized['one_frame']['relevant_trace_bytes'] = old['one_frame']['relevant_trace_bytes']
    assert normalized == old, 'dense mode changed something besides its output policy and trace length'
    trace = base / 'nq01-area/relevant-frames.jsonl'
    assert 0 < trace.stat().st_size == area['one_frame']['relevant_trace_bytes'] <= registration['trace_limit_bytes']
    rows = [json.loads(line) for line in trace.read_text().splitlines()]
    assert all(len(row) == 9 and len(row[-1]) == 6 for row in rows)
    assert all(a[0] < b[0] for a, b in zip(rows, rows[1:]))
    assert all(row[1] in (0x12, 0x14) or row[5] != 0 or any(slot[3] & 64 for slot in row[8]) for row in rows)
    assert rows[-1][0] == area['one_frame']['route_frames']
    assert rows[-1][1] == area['one_frame']['raw_endpoint']['area']
    charge = 0
    cases = []
    for case in registration['cases']:
        path = base / case['id'] / 'summary.json'
        summary = read(path)
        assert summary['verified_replays'] == 3
        actual = summary['ordinary']['physical_frames_including_setup'] + 2 * summary['one_frame']['physical_frames_including_setup']
        assert actual == registration['expected_physical_frames_per_case']
        for key in ('input_sha256', 'rom_sha256', 'core_sha256'):
            assert summary[key] == registration[key]
        cases.append({'id': case['id'], 'summary_sha256': sha(path),
                      'trace_sha256': sha(path.with_name('relevant-frames.jsonl')),
                      'physical_frames_including_setup': actual,
                      'summary': summary})
        charge += actual
    assert charge <= registration['auxiliary_frame_ceiling']
    report = {'format': 'depth-transfer-nq01-results-v1', 'decision': 'pass',
              'verified_utc': datetime.now(timezone.utc).isoformat(),
              'registration_sha256': sha(base / 'nq01-registration.json'),
              'checker_sha256': sha(Path(__file__)), 'cases': cases,
              'known_auxiliary_frames': charge, 'dense_trace_rows': len(rows),
              'dense_trace_bytes': trace.stat().st_size,
              'interpretation': 'Existing route only: default byte compatibility and dense-trace neutrality, not new search or encounter success.'}
    assert not args.out.exists()
    args.out.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({k: report[k] for k in ('decision', 'known_auxiliary_frames', 'dense_trace_rows', 'dense_trace_bytes')}))


if __name__ == '__main__':
    main()
