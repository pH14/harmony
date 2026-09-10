#!/usr/bin/env python3
"""Fixed RR01 local-loss query; no inference of lifetime damage or useful futures."""
import argparse
from collections import Counter
from dataclasses import asdict
import hashlib
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'endpoint-encounter'))
from score_checkpoint_inspection import slots_for

RESOURCE_FIELDS = ('equipment', 'bosses', 'missile_capacity', 'energy_tanks', 'health', 'missiles')
IDENTITY_FIELDS = ('area', 'offset', 'data_index', 'attributes')


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def qualified_slots(context):
    result = []
    for slot in slots_for(context):
        if slot is not None:
            row = asdict(slot)
            if row['hp'] == 255:
                row['hp'] = None
            result.append(row)
    return result


def assess(row):
    assert type(row['replaces']) is bool
    candidate, incumbent = row['candidate'], row['incumbent']
    if incumbent is None:
        return dict(reason='missing_incumbent', lower_hp_lost=None)
    for endpoint in (candidate, incumbent):
        assert endpoint['classified_slots'] == qualified_slots(endpoint['context'])
    if not all(e['alive'] is True and e['victory'] is False for e in (candidate, incumbent)):
        return dict(reason='not_two_live_nonvictory_states', lower_hp_lost=None)
    if any(not e['classified_slots'] for e in (candidate, incumbent)):
        return dict(reason='unclassified', lower_hp_lost=None)
    if any(len(e['classified_slots']) != 1 for e in (candidate, incumbent)):
        return dict(reason='multiple_classified_slots', lower_hp_lost=None)
    a, b = candidate['classified_slots'][0], incumbent['classified_slots'][0]
    if any(s['hp'] is None for s in (a, b)):
        return dict(reason='hp_unavailable', lower_hp_lost=None)
    if tuple(a[k] for k in IDENTITY_FIELDS) != tuple(b[k] for k in IDENTITY_FIELDS):
        return dict(reason='different_classified_identity', lower_hp_lost=None)
    loser, winner = (incumbent, candidate) if row['replaces'] else (candidate, incumbent)
    lost, kept = (b, a) if row['replaces'] else (a, b)
    equal = all(loser['state'][k] == winner['state'][k] for k in RESOURCE_FIELDS)
    return dict(reason='comparable', lower_hp_lost=lost['hp'] < kept['hp'],
                loser_hp=lost['hp'], winner_hp=kept['hp'], equal_resources=equal,
                direction='replaced_incumbent' if row['replaces'] else 'rejected_candidate')


def score(protocol, output):
    master = json.loads((protocol / 'rr01-registration.json').read_text())
    process = json.loads((output / 'rr01-process.json').read_text())
    assert process['registration_sha256'] == sha(protocol / 'rr01-registration.json')
    assert process['decision'] == 'ready_for_offline_scoring'
    assert process['admitted_search_frames'] == 0
    assert process['cpus'] == master['cpus']
    assert process['cgroup_limits'] == master['cgroup_limits']
    for name, digest in process['files'].items():
        path = (output / name).resolve()
        assert output.resolve() in path.parents
        assert sha(path) == digest, name
    assert [p['phase'] for p in process['phases']] == ['qualify', 'inspect']
    for phase, jobs, frames in zip(process['phases'], (4, 2548), (865, 250267)):
        assert phase['exit_code'] == 0 and phase['killed'] is None
        report = phase['result']
        assert report['status'] == 'complete'
        assert report['binding']['executable_sha256'] == master['binary_sha256']
        assert (report['verified_jobs'], report['verified_job_frames']) == (jobs, frames)
        assert report['known_inspector_setup_frames'] == 929
        assert report['known_replay_frames'] == frames
    report = process['phases'][1]['result']
    assert report['verified_original_final_checkpoint'] is True
    assert report['inspected_competitions'] == master['expected_competitions'] == 3770
    counts = Counter(competitions=0, comparable=0, lower_hp_lost=0, equal_resource_lower_hp_lost=0)
    strata, earliest = Counter(), {}
    previous = (0, 0)
    with (output / 'inspect/contexts.jsonl').open() as stream:
        for ordinal, line in enumerate(stream, 1):
            row = json.loads(line)
            assert row['ordinal'] == ordinal
            assert 1 <= row['execution'] <= 2548
            stamp = row['execution'], ordinal
            assert stamp > previous
            previous = stamp
            finding = assess(row)
            counts['competitions'] += 1
            counts[finding['reason']] += 1
            if finding['lower_hp_lost'] is True:
                counts['lower_hp_lost'] += 1
                strata[finding['direction']] += 1
                counts['equal_resource_lower_hp_lost'] += finding['equal_resources']
                for category in ['any'] + (['equal_resources'] if finding['equal_resources'] else []):
                    earliest.setdefault(category, dict(ordinal=ordinal, execution=row['execution'],
                        incumbent_id=row['incumbent_id'], finding=finding,
                        candidate_input_sha256=row['candidate_input_sha256'],
                        incumbent_input_sha256=row['incumbent_input_sha256']))
    assert counts['competitions'] == 3770
    assert process['known_auxiliary_frames'] == 252990
    return dict(format='retention-replay-rr01-analysis-v1', counts=dict(counts), strata=dict(strata),
                earliest=earliest, known_auxiliary_frames=process['known_auxiliary_frames'],
                admitted_search_frames=0,
                decision=('consider_earliest_local_counterexample' if counts['lower_hp_lost'] else
                    'no_qualifying_loss_in_comparable_rows' if counts['comparable'] else
                    'no_comparable_rows_measurement_unavailable'),
                scope='Exact fixed pilot only. Unavailable rows stay unknown. No lifetime damage, global coverage, defeat or retention-utility claim. A positive observation still needs a paired continuation counterexample before policy changes.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--protocol', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    result = score(args.protocol, args.output)
    with args.out.open('x') as stream:
        json.dump(result, stream, indent=2)
        stream.write('\n')
