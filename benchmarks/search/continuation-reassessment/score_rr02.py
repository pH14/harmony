#!/usr/bin/env python3
"""Unchanged local-loss query, explicitly scoped to the reconstructed ms02 run."""
import argparse
from collections import Counter
import gzip
import json
from pathlib import Path
from score_rr01 import assess, sha


def compare_root(original, actual, original_core, runtime_core):
    blobs = []
    for value, core in ((original, original_core), (actual, runtime_core)):
        blob = bytes(value['emulator_state'])
        assert len(blob) >= 120
        assert blob[:8] == b'HQNESST2'
        assert blob[8:48] == b'26bb785c9deddb66a17717b21bb4e328f03ade32'
        assert blob[48:112] == core.encode('ascii')
        assert int.from_bytes(blob[112:120], 'little') == len(blob) - 120
        blobs.append(blob)
    assert blobs[0][:48] == blobs[1][:48]
    assert blobs[0][112:] == blobs[1][112:]
    assert {k: v for k, v in original.items() if k != 'emulator_state'} == {
        k: v for k, v in actual.items() if k != 'emulator_state'}


def score(protocol, output):
    master = json.loads((protocol / 'rr02-registration.json').read_text())
    process = json.loads((output / 'rr02-process.json').read_text())
    assert process['registration_sha256'] == sha(protocol / 'rr02-registration.json')
    assert process['decision'] == 'ready_for_offline_scoring'
    assert process['admitted_search_frames'] == 0
    assert process['cpus'] == master['cpus']
    assert process['cgroup_limits'] == master['cgroup_limits']
    for name, digest in master['protocol_sha256'].items():
        assert sha(protocol / name) == digest, name
    repo = protocol.parents[2]
    for name, digest in master['scorer_dependency_sha256'].items():
        assert sha(repo / name) == digest, name
    for name, digest in process['files'].items():
        path = (output / name).resolve()
        assert output.resolve() in path.parents
        assert sha(path) == digest, name
    assert [p['phase'] for p in process['phases']] == ['qualify', 'inspect']
    request = json.loads((protocol / 'rr02-qualify-request.json').read_text())
    ap01 = protocol.parent / 'endpoint-encounter/ap01-output'
    original_root = json.loads(gzip.decompress((ap01 / 'root-snapshot.json.gz').read_bytes()))
    original_stream = gzip.decompress((ap01 / 'stream.jsonl.gz').read_bytes())
    header, body = original_stream.split(b'\n', 1)
    original_header = json.loads(header)
    generated = json.loads((output / 'inspect-request.json').read_text())
    expected = json.loads((protocol / 'rr02-inspect-template.json').read_text())
    expected['qualification'] = dict(path=generated['qualification']['path'],
                                     sha256=sha(output / 'qualify/result.json'))
    assert generated == expected
    for phase, jobs, frames in zip(process['phases'], (4, 2548), (865, 250267)):
        assert phase['exit_code'] == 0 and phase['killed'] is None
        report = phase['result']
        assert report['status'] == 'complete'
        assert report['format'] == 'metroid-retention-replay-v2'
        assert report['snapshot_comparison_policy'] == 'quicknes-exact-snapshot-except-verified-core-v1'
        assert report['known_reconstruction_frames'] == master['reconstruction_frames']
        assert report['binding']['reconstruction']['input_sha256'] == master['reconstruction_input_sha256']
        assert report['binding']['reconstruction']['actions'] == master['reconstruction_actions']
        assert report['binding']['executable_sha256'] == master['binary_sha256']
        assert (report['verified_jobs'], report['verified_job_frames']) == (jobs, frames)
        assert report['known_inspector_setup_frames'] == 929
        assert report['known_replay_frames'] == frames
        name = phase['phase']
        assert report == json.loads((output / name / 'result.json').read_text())
        request_path = protocol / 'rr02-qualify-request.json' if name == 'qualify' else output / 'inspect-request.json'
        assert report['request_sha256'] == phase['request_sha256'] == sha(request_path)
        assert report['root']['context'] == request['expected_root_context']
        assert report['root']['alive'] is True and report['root']['victory'] is False
        compare_root(original_root, json.loads((output / name / 'runtime-root-snapshot.json').read_text()),
                     request['original_core_sha256'], request['core']['sha256'])
        assert sha(output / name / 'runtime-origin.bin') == report['runtime_origin_sha256']
        runtime_header, selected = (output / name / 'derived-stream.jsonl').read_bytes().split(b'\n', 1)
        expected_header = dict(original_header)
        expected_header['emulator_backend'] = expected_header['emulator_backend'].replace(request['original_core_sha256'], request['core']['sha256'])
        expected_header['origin_checkpoint_sha256'] = report['runtime_origin_sha256']
        assert json.loads(runtime_header) == expected_header
        assert selected == b''.join(body.splitlines(keepends=True)[:jobs])
    report = process['phases'][1]['result']
    assert report['verified_original_final_checkpoint'] is False
    assert report['verified_final_checkpoint_correspondence'] is True
    assert report['verified_checkpoint_entries'] == master['expected_final_checkpoint_entries'] == 588
    qualification = process['phases'][0]['result']
    assert report['binding'] == qualification['binding']
    assert report['runtime_origin_sha256'] == qualification['runtime_origin_sha256']
    assert report['root']['snapshot_sha256'] == qualification['root']['snapshot_sha256']
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
    assert process['known_auxiliary_frames'] == master['expected_known_auxiliary_frames_if_both_pass'] == 488740
    return dict(format='retention-replay-rr02-analysis-v1', counts=dict(counts), strata=dict(strata),
                earliest=earliest, known_auxiliary_frames=process['known_auxiliary_frames'],
                admitted_search_frames=0,
                decision=('consider_earliest_local_counterexample' if counts['lower_hp_lost'] else
                    'no_qualifying_loss_in_comparable_rows' if counts['comparable'] else
                    'no_comparable_rows_measurement_unavailable'),
                scope='Reconstructed ms02 campaign only; identity of unrecorded discarded ARM states is unproved. Unavailable rows stay unknown. No lifetime damage, global coverage, defeat or retention-utility claim. A positive observation still needs a paired continuation counterexample before policy changes.')


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
