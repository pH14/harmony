#!/usr/bin/env python3
"""Summarize the fixed E03 tape panel, retaining failed or unavailable evidence."""
import argparse
import hashlib
import json
from collections import Counter
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
    base = args.root / 'depth-transfer-20260909'
    reg = read(base / 'e03-registration.json')
    records, total = [], 0
    for case in reg['cases']:
        folder = base / case['id']
        path = folder / 'summary.json'
        record = {'id':case['id'],'source_run':case['source_run'],'input_sha256':case['input_sha256']}
        if not path.exists():
            completed = [read(folder / name) for name in ('ordinary.json','first.json') if (folder / name).exists()]
            frames = sum(s['physical_frames_including_setup'] for s in completed)
            record.update(complete=False,known_complete_pass_frames=frames,
                          partial_emulator_advancement='unknown; retain the reserved full-case ceiling',
                          log_sha256=sha(base / (case['id'] + '.log')),
                          last_log_lines=(base / (case['id'] + '.log')).read_text().splitlines()[-4:])
            total += frames
            records.append(record)
            continue
        s = read(path)
        assert s['format'] == 'metroid-boss-memory-probe-v3' and s['verified_replays'] == 3
        for key in ('rom_sha256','core_sha256'):
            assert s[key] == reg[key]
        assert s['input_sha256'] == case['input_sha256']
        assert s['one_frame']['route_frames'] == case['route_frames']
        frames = s['ordinary']['physical_frames_including_setup'] + 2*s['one_frame']['physical_frames_including_setup']
        assert frames == case['expected_physical_frames_including_three_setups']
        trace = folder / 'relevant-frames.jsonl'
        assert trace.stat().st_size == s['one_frame']['relevant_trace_bytes'] <= reg['trace_limit_bytes']
        counts, first, defeats, previous = Counter(), {}, {}, None
        for line in trace.read_text().splitlines():
            row = json.loads(line)
            assert len(row) == 11
            if row[2] == 3 and row[1] in (18,20):
                counts[row[1]] += 1
                first.setdefault(row[1],row[0])
                if previous is not None and previous[0]+1 == row[0] and previous[1] == row[1]:
                    for name,area,column,mask in [('kraid',18,6,1),('ridley',20,7,2)]:
                        if row[1] == area and not previous[column]&mask and row[column]&mask:
                            defeats.setdefault(name,row[0])
            previous = row
        d = s['one_frame']['context_diagnostics']
        assert d['verified_self_restores'] == 0
        if d['comparable_intervals'] == 0:
            assert d['observed_hp_loss'] is None
        record.update(complete=True,known_physical_frames=frames,summary_sha256=sha(path),trace_sha256=sha(trace),
                      boss_area_gameplay_frames=dict(counts),first_boss_area_gameplay_frame=first,
                      guarded_defeat_transitions=defeats,context_diagnostics=d,
                      route_endpoint=s['one_frame']['endpoint'])
        total += frames
        records.append(record)
    assert total <= reg['auxiliary_frame_ceiling']
    complete = all(r['complete'] for r in records)
    if complete:
        assert total == reg['expected_known_physical_frames']
    result = {'format':'depth-transfer-e03-results-v1','complete':complete,
              'registration_sha256':sha(base/'e03-registration.json'),'checker_sha256':sha(Path(__file__)),
              'known_auxiliary_frames':total,'records':records,
              'tapes_with_classified_observations':sum(r.get('context_diagnostics',{}).get('classified_frames',0)>0 for r in records),
              'tapes_with_hp_drop':sum(r.get('context_diagnostics',{}).get('first_hp_drop_route_frame') is not None for r in records),
              'interpretation':'Selected surviving development tapes only. A missing classification is unavailable HP evidence; it does not establish campaign-wide absence or a search-policy effect.'}
    assert not args.out.exists()
    args.out.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({key:result[key] for key in ('complete','known_auxiliary_frames','tapes_with_classified_observations','tapes_with_hp_drop')}))


if __name__ == '__main__':
    main()
