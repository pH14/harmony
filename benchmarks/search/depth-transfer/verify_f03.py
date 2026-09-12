#!/usr/bin/env python3
"""Verify saved-byte evidence and model output without advancing an emulator."""
import csv
import gzip
import hashlib
import io
import json
from collections import Counter, defaultdict
from pathlib import Path

from boss_interval import BossIntervalObserver

ROOT = Path(__file__).resolve().parent


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main():
    result = json.loads((ROOT / 'f03-results.json').read_text())
    raw = gzip.decompress((ROOT / 'f03-boss-area-frames.csv.gz').read_bytes())
    rows = list(csv.DictReader(io.StringIO(raw.decode())))
    assert len(rows) == 28522
    assert digest((ROOT / 'f03-registration.json').read_bytes()) == result['registration_sha256']
    for run in result['runs'].values():
        assert run['summary']['complete'] and run['summary']['advanced_frames'] == 120540
        assert run['trace_sha256'] == digest(raw)
    assert result['known_script_advanced_frames'] == 241080
    old = gzip.decompress((ROOT / 'f02-boss-area-frames.csv.gz').read_bytes())
    keys = next(csv.reader(io.StringIO(old.decode())))
    stream = io.StringIO()
    writer = csv.DictWriter(stream, keys, extrasaction='ignore', lineterminator='\n')
    writer.writeheader()
    writer.writerows(rows)
    assert stream.getvalue().encode() == old
    fixture = (ROOT / 'f03-fight-slot0.csv').read_bytes()
    keys = next(csv.reader(io.StringIO(fixture.decode())))
    stream = io.StringIO()
    writer = csv.DictWriter(stream, keys, extrasaction='ignore', lineterminator='\n')
    writer.writeheader()
    writer.writerows(r for r in rows if 75811 <= int(r['frame']) <= 76391 or 104837 <= int(r['frame']) <= 105236)
    assert stream.getvalue().encode() == fixture
    observer, counts, losses = BossIntervalObserver(), Counter(), defaultdict(lambda: [0, 0])
    for row in rows:
        row = {k: int(v) for k, v in row.items()}
        for event in observer.observe(0, row['frame'], row)['intervals']:
            counts[event['kind']] += 1
            if event['kind'] == 'hp_drop':
                losses[event['area']][0] += 1
                losses[event['area']][1] += event['hp_loss']
    assert dict(losses) == {20: [62, 140], 18: [36, 96]}
    assert counts == Counter(continuous=879, hp_drop=98, baseline=2, left_or_unclassified=2)
    report = {'format': 'depth-transfer-f03-analysis-v1', 'decision': 'pass_for_observed_reference_lifetimes',
              'raw_trace_sha256': digest(raw), 'raw_trace_bytes': len(raw), 'raw_rows': len(rows),
              'old_column_projection_equals_f02': True, 'rust_fixture_equals_raw_projection': True,
              'interval_counts': dict(counts), 'hp_decrease_count_and_sum_by_area': dict(losses),
              'known_auxiliary_frames': 241080,
              'file_sha256': {name: digest((ROOT / name).read_bytes()) for name in (
                  'f03-registration.json', 'f03-results.json', 'f03-fight-slot0.csv',
                  'boss_interval.py', 'test_boss_interval.py', 'verify_f03.py')},
              'limitations': ['Two public-reference fights; no fresh-search success.',
                             'Offline observer cuts are not actual native emulator restores.',
                             'Observed HP decreases do not prove absence of invisible same-key reloads.']}
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
