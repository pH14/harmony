#!/usr/bin/env python3
"""Check completed native CD01 pairs against their frozen identities and limits.

This is artifact/resource verification, not a replacement for score_cd01.py.
It reads saved evidence only and does not execute or replay the emulator.
"""
import argparse
from datetime import datetime
import hashlib
import json
from pathlib import Path
import tarfile


def sha(data):
    return hashlib.sha256(data).hexdigest()


def verify(protocol, evidence, pair):
    name = f'cd01-pair-{pair}'
    master_path = protocol / 'cd01-registration.json'
    master = json.loads(master_path.read_text())
    spec = next(p for p in master['panels'] if p['id'] == name)
    sub_path = protocol / spec['registration']
    sub = json.loads(sub_path.read_text())
    assert sha(sub_path.read_bytes()) == spec['registration_sha256']
    panel_path = evidence / f'{name}-results.json'
    panel = json.loads(panel_path.read_text())
    assert panel['registration_sha256'] == spec['registration_sha256']
    assert panel['registration_commit'] == '648a70474ea7477c4c3d41969f47a484b3813549'
    assert panel['execution_complete'] and panel['allocation_stop'] is None
    assert [r['id'] for r in panel['records']] == [c['id'] for c in sub['cells']]
    journal_path = evidence / f'{name}-journal.jsonl'
    journal = [json.loads(line) for line in journal_path.read_text().splitlines()]
    starts = [j for j in journal if j.get('MESSAGE', '').startswith('Started ')]
    ends = [j for j in journal if j.get('MESSAGE', '').endswith('Deactivated successfully.')]
    usage = [j for j in journal if 'CPU_USAGE_NSEC' in j]
    assert len(starts) == len(ends) == len(usage) == 1
    assert starts[0]['INVOCATION_ID'] == ends[0]['INVOCATION_ID'] == usage[0]['INVOCATION_ID']
    service_seconds = (int(ends[0]['__REALTIME_TIMESTAMP']) - int(starts[0]['__REALTIME_TIMESTAMP'])) / 1e6
    assert 0 < service_seconds <= spec['service_runtime_seconds']
    assert int(usage[0]['MEMORY_PEAK']) <= 12 * 1024**3
    first_cpu, last_cpu = map(int, spec['cpus'].split('-'))
    cpus = list(range(first_cpu, last_cpu + 1))
    archive_path = evidence / f'{name}-native.tar.gz'
    records, members = [], []
    with tarfile.open(archive_path) as archive:
        def raw(path):
            member = archive.getmember(path)
            assert member.isfile()
            return archive.extractfile(member).read()

        assert raw(f'{name}/results.json') == panel_path.read_bytes()
        for member in archive.getmembers():
            assert member.name.startswith(name + '/') or member.name == name
            assert '..' not in Path(member.name).parts
            assert member.isfile() or member.isdir()
            if member.isfile():
                members.append({'path': member.name, 'bytes': member.size, 'sha256': sha(raw(member.name))})
        for record, cell in zip(panel['records'], sub['cells']):
            assert record['checks_passed'] and record['exit_code'] == 0
            summary = record['summary']
            result = summary['result']
            base = f'{name}/{record["id"]}/{summary["cell"]}/'
            summary_raw = raw(base + 'summary.json')
            assert sha(summary_raw) == record['summary_sha256']
            assert json.loads(summary_raw) == summary
            assert json.loads(raw(base + 'campaign/result.json')) == result
            assert summary['status'] == result['status'] == 'complete'
            assert summary['host']['hostname'] == spec['host'] == 'ms02'
            assert summary['cpu_set'] == cpus
            assert summary['build']['binary_sha256'] == sub['binary_sha256']
            assert summary['build']['source']['source_tree_sha256'] == sub['source_tree_sha256']
            assert summary['build']['source']['source_commit'] == sub['source_commit']
            for key, expected in cell['expected_identity'].items():
                assert summary['identity'][key] == expected, key
            assert summary['origin'] == 'power-on new-game genesis'
            assert datetime.fromisoformat(record['started_utc']) >= datetime.fromisoformat(master['registered_utc'])
            assert datetime.fromisoformat(record['finished_utc']) <= datetime.fromisoformat(master['deadline_utc'])
            record_seconds = (datetime.fromisoformat(record['finished_utc']) - datetime.fromisoformat(record['started_utc'])).total_seconds()
            assert 0 < summary['elapsed_seconds'] <= record_seconds <= cell['subprocess_timeout_seconds']
            assert summary['max_process_rss_bytes'] <= 12 * 1024**3
            assert summary['peak_process_tree_rss_bytes_sampled'] <= 12 * 1024**3
            assert summary['peak_disk_allocated_bytes_sampled'] <= cell['disk_limit_gib'] * 1024**3
            assert result['verification'] == 'witness' and result['stop_reason'] in cell['allowed_stops']
            assert result['frames_emulated'] <= master['horizon_frames'] + master['bounded_inflight_drain_per_cell']
            assert result['executions'] <= cell['expected_identity']['executions'] + 8
            progress_raw = raw(base + 'campaign/progress.jsonl')
            assert sha(progress_raw) == record['progress_sha256']
            previous = (0, 0)
            count = 0
            for line in progress_raw.splitlines():
                progress = json.loads(line)
                current = (progress['executions'], progress['frames_emulated'])
                assert current[0] >= previous[0] and current[1] >= previous[1]
                previous = current
                count += 1
            assert previous == (result['executions'], result['frames_emulated'])
            witness_hashes = {}
            for milestone, witness in result['milestone_witnesses'].items():
                digest = sha(raw(base + f'campaign/milestone-inputs/{milestone}.json'))
                assert digest == witness['input_sha256']
                witness_hashes[milestone] = digest
            endpoint = record['endpoint_evidence']
            if endpoint['hit_by_budget']:
                witness = result['milestone_witnesses']['bombs']['replay']
                assert not witness['dead']
                assert witness['diagnostics']['named_progress']['first_seen']['bombs'] is not None
                assert result['first_milestone']['frames_emulated'] == endpoint['arrival_exact']
            records.append({'id': record['id'], 'cpu_set': cpus, 'progress_records': count,
                            'record_elapsed_seconds': record_seconds,
                            'elapsed_seconds': summary['elapsed_seconds'], 'cpu_seconds': summary['cpu_seconds'],
                            'max_process_rss_bytes': summary['max_process_rss_bytes'],
                            'peak_disk_allocated_bytes_sampled': summary['peak_disk_allocated_bytes_sampled'],
                            'witness_sha256': witness_hashes})
    return {'format': 'continuation-cd01-native-verification-v1', 'pair': pair, 'verified': True,
            'registration_sha256': sha(master_path.read_bytes()), 'panel_sha256': sha(panel_path.read_bytes()),
            'native_archive_sha256': sha(archive_path.read_bytes()), 'journal_sha256': sha(journal_path.read_bytes()),
            'service_invocation': starts[0]['INVOCATION_ID'], 'service_elapsed_seconds': service_seconds,
            'service_cpu_nanoseconds': int(usage[0]['CPU_USAGE_NSEC']),
            'service_memory_peak_bytes': int(usage[0]['MEMORY_PEAK']), 'records': records, 'members': members,
            'limits': 'Offline integrity and registered resource checks only. Use the unchanged frozen scorer after each complete wave for allocation; this report does not authorize a wave.'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--protocol', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    parser.add_argument('--pair', type=int, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    result = verify(args.protocol, args.evidence, args.pair)
    args.out.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({k: result[k] for k in ('pair', 'verified', 'service_elapsed_seconds', 'service_memory_peak_bytes')}))
