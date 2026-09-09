#!/usr/bin/env python3
"""Recheck the public observer evidence from stored traces; runs no emulator."""
import csv
import gzip
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def sha(data):
    return hashlib.sha256(data).hexdigest()


def read(name):
    return json.loads((ROOT / name).read_text())


def main():
    for panel, registration, script in (
            ('f01r', 'f01r-observer-registration.json', 'observe_fm2_v2.lua'),
            ('f02', 'f02-lifecycle-registration.json', 'observe_fm2_lifecycle.lua')):
        result = read(panel + '-results.json')
        assert result['registration_sha256'] == sha((ROOT / registration).read_bytes())
        assert read(registration)['lua_sha256'] == sha((ROOT / script).read_bytes())
        a, b = result['runs'].values()
        assert a['summary'] == b['summary'] and a['trace_sha256'] == b['trace_sha256']
        assert a['summary']['complete'] and a['summary']['advanced_frames'] == 120540
    with gzip.open(ROOT / 'f02-boss-area-frames.csv.gz', 'rb') as stream:
        raw = stream.read(32 * 1024**2 + 1)
    assert len(raw) <= 32 * 1024**2
    analysis = read('f02-lifecycle-analysis.json')
    assert sha(raw) == analysis['raw_trace_sha256'] == read('f02-results.json')['runs']['f02-a']['trace_sha256']
    assert sha((ROOT / 'f02-boss-area-frames.csv.gz').read_bytes()) == analysis['gzip_sha256']
    lines = raw.decode().splitlines()
    rows = [{key: int(value) for key, value in row.items()} for row in csv.DictReader(lines)]
    previous_flags = (0, 0)
    projected = [lines[0]]
    for row, line in zip(rows, lines[1:]):
        flags = (row['kraid'], row['ridley'])
        if (row['loader'] != 0 or flags != previous_flags
                or any(row[f'slot{i}_special'] & 64 for i in range(6))):
            projected.append(line)
        previous_flags = flags
    old = (ROOT / 'f01r-relevant-frames.csv').read_bytes()
    assert projected == old.decode().splitlines()
    assert sha(old) == read('f01r-results.json')['runs']['f01r-a']['trace_sha256']
    for episode in analysis['episodes']:
        name, area = episode['boss'], episode['area']
        mask = {'kraid': 1, 'ridley': 2}[name]
        start = next(row['frame'] for row in rows if row['area'] == area and row['mode'] == 3
                     and row['loader'] == 1 and row['slot0_special'] & 64 and row['slot0_status'] != 0)
        end = next(row['frame'] for row in rows if row['area'] == area and row['frame'] >= start
                   and row[name] & mask)
        selected = [row for row in rows if row['area'] == area and start <= row['frame'] <= end]
        assert start == episode['entry_frame'] and end == episode['defeat_frame']
        assert len(selected) == episode['consecutive_observed_frames'] == end - start + 1
        assert {row['slot0_type'] for row in selected} == {episode['unchanged_data_index']}
        assert all(row['slot0_status'] != 0 for row in selected[:-1])
        assert selected[-1]['slot0_status'] == 0
        assert selected[0]['slot0_hp'] == episode['initial_hp']
        assert selected[-1]['slot0_hp'] == episode['final_hp'] == 0
        assert next(row['frame'] for row in selected if row['slot0_hp'] == 0) == episode['first_zero_hp_frame']
        drops = []
        for previous, current in zip(selected, selected[1:]):
            assert current['frame'] == previous['frame'] + 1
            assert current['slot0_hp'] <= previous['slot0_hp']
            if current['slot0_hp'] < previous['slot0_hp']:
                assert current['slot0_status'] in (3, 6)
                assert current['loader'] == 0 and current['slot0_special'] & 64 == 0
                drops.append((current['frame'], previous['slot0_hp'] - current['slot0_hp']))
        assert [frame for frame, _ in drops] == episode['drop_frames']
        assert len(drops) == episode['hp_drop_events'] == episode['drops_without_loader'] == episode['drops_without_special_tag']
        assert sum(delta for _, delta in drops) == episode['hp_drop_sum'] == episode['initial_hp']
    print('Verified both repeated controls, old-filter projection, and 98 missed HP changes across two complete lifetimes.')


if __name__ == '__main__':
    main()
