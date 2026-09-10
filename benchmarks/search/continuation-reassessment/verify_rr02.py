#!/usr/bin/env python3
"""Recompute RR02 from compressed native artifacts; no emulator execution."""
import gzip
import hashlib
import json
from pathlib import Path
import tempfile
from datetime import datetime

from score_rr02 import assess, score, sha

HERE = Path(__file__).resolve().parent


def verify():
    master = json.loads((HERE / 'rr02-registration.json').read_text())
    manifest = json.loads((HERE / 'rr02-evidence-manifest.json').read_text())
    build = json.loads((HERE / 'rr02-build-info.json').read_text())
    assert build['exit_code'] == 0 and build['emulator_executed'] is False
    assert build['source_commit'] == master['source_commit']
    assert build['binary_sha256'] == master['binary_sha256']
    assert build['archive_sha256'] == master['source_archive_sha256']
    assert sum(item['raw_bytes'] for item in manifest['files'].values()) < master['output_limit_bytes']
    with tempfile.TemporaryDirectory(prefix='harmony-rr02-verify-') as tmp:
        output = Path(tmp).resolve()
        for name, item in manifest['files'].items():
            archive = (HERE / item['gzip']).resolve()
            target = (output / name).resolve()
            assert HERE in archive.parents and output in target.parents
            assert archive.stat().st_size == item['gzip_bytes'] and sha(archive) == item['gzip_sha256']
            target.parent.mkdir(parents=True, exist_ok=True)
            count, digest = 0, hashlib.sha256()
            with gzip.open(archive, 'rb') as source, target.open('xb') as stream:
                while block := source.read(1024 * 1024):
                    count += len(block)
                    assert count <= item['raw_bytes']
                    digest.update(block)
                    stream.write(block)
            assert count == item['raw_bytes'] and digest.hexdigest() == item['raw_sha256']
        analysis = score(HERE, output)
        assert analysis == json.loads((HERE / 'rr02-analysis.json').read_text())
        contexts = [json.loads(line) for line in (output / 'inspect/contexts.jsonl').read_text().splitlines()]
        positive = [row for row in contexts if assess(row)['lower_hp_lost'] is True]
        dependence = json.loads((HERE / 'rr02-dependence.json').read_text())
        assert dependence['positive_events'] == len(positive)
        for label, values in {
            'unique_candidate_snapshot_hashes': {r['candidate']['snapshot_sha256'] for r in positive},
            'unique_candidate_input_hashes': {r['candidate_input_sha256'] for r in positive},
            'unique_incumbent_snapshot_hashes': {r['incumbent']['snapshot_sha256'] for r in positive},
            'unique_snapshot_pairs': {(r['candidate']['snapshot_sha256'], r['incumbent']['snapshot_sha256']) for r in positive},
        }.items():
            assert dependence[label] == len(values)
        earliest = json.loads((HERE / 'rr02-earliest-pair.json').read_text())
        assert earliest['record'] == positive[0]
        assert earliest['record']['ordinal'] == analysis['earliest']['equal_resources']['ordinal'] == 692
        assert earliest['capture_file'] == manifest['files']['inspect/competitions.bin']
        process = json.loads((output / 'rr02-process.json').read_text())
        assert datetime.fromisoformat(master['registered_utc']) <= datetime.fromisoformat(process['started_utc'])
        assert datetime.fromisoformat(process['ended_utc']) < datetime.fromisoformat(master['deadline_utc'])
        assert process['hostname'] == 'ms02'
        requests = [json.loads((HERE / 'rr02-qualify-request.json').read_text()),
                    json.loads((output / 'inspect-request.json').read_text())]
        assert requests[1]['qualification']['path'] == master['output'] + '/qualify/result.json'
        for phase, request in zip(process['phases'], requests):
            assert phase['command'][0] == master['binary']
            assert phase['wall_seconds'] < request['wall_seconds']
            report = phase['result']
            assert report['physical_frame_ceiling'] == request['physical_frame_ceiling']
            assert report['binding']['original_stream_sha256'] == request['stream']['sha256']
            assert report['binding']['origin_sha256'] == request['origin']['sha256']
            assert report['binding']['final_checkpoint_sha256'] == request['final_checkpoint']['sha256']
            assert report['binding']['root_snapshot_sha256'] == request['root_snapshot_sha256']
            assert report['binding']['rom_sha256'] == request['rom']['sha256']
            assert report['binding']['runtime_core_sha256'] == request['core']['sha256']
            assert report['binding']['original_core_sha256'] == request['original_core_sha256']
        state = dict(line.split('=', 1) for line in (output / 'service-state.txt').read_text().splitlines())
        assert state == dict(MainPID='0', ActiveState='inactive', SubState='dead')
        journal = [json.loads(line) for line in (output / 'service-journal.jsonl').read_text().splitlines()]
        invocation = process['invocation_id']
        journal = [row for row in journal if isinstance(row, dict)]
        assert all(row.get('INVOCATION_ID', row.get('_SYSTEMD_INVOCATION_ID')) == invocation for row in journal)
        assert any('Deactivated successfully' in str(row.get('MESSAGE', '')) for row in journal)
        assert not any(row.get('UNIT_RESULT') or row.get('EXIT_STATUS') for row in journal)
        resources = [row for row in journal if 'CPU_USAGE_NSEC' in row]
        assert len(resources) == 1
        memory_peak = int(resources[0]['MEMORY_PEAK'])
        assert memory_peak <= int(master['cgroup_limits']['memory.max'])
        assert resources[0]['MEMORY_SWAP_PEAK'] == '0'
        ledger = json.loads((HERE / 'ledger-after-rr02.json').read_text())
        previous = json.loads((HERE / 'ledger-after-rr01.json').read_text())
        for field, name in {
            'prior_ledger_sha256': 'ledger-after-rr01.json',
            'registration_sha256': 'rr02-registration.json',
            'analysis_sha256': 'rr02-analysis.json',
            'evidence_manifest_sha256': 'rr02-evidence-manifest.json',
            'closure_verification_sha256': 'rr02-closure-verification.json',
        }.items():
            assert ledger[field] == sha(HERE / name)
        assert ledger['known_auxiliary_frames'] == analysis['known_auxiliary_frames']
        assert ledger['known_auxiliary_frames'] == (ledger['known_inspector_setup_frames']
            + ledger['known_reconstruction_frames'] + ledger['verified_campaign_replay_frames'])
        assert ledger['cumulative_since_user_resumption_known_auxiliary_frames'] == (
            previous['cumulative_since_user_resumption_known_auxiliary_frames'] + ledger['known_auxiliary_frames'])
        assert ledger['cumulative_since_user_resumption_admitted_search_frames'] == previous['cumulative_since_user_resumption_admitted_search_frames']
        return dict(format='retention-reconstruction-rr02-closure-v1',
                    decision=analysis['decision'], service_invocation=invocation,
                    controller_elapsed_seconds=(datetime.fromisoformat(process['ended_utc'])
                        - datetime.fromisoformat(process['started_utc'])).total_seconds(),
                    service_cpu_nanoseconds=int(resources[0]['CPU_USAGE_NSEC']),
                    service_memory_peak_bytes=memory_peak, service_state=state,
                    known_auxiliary_frames=analysis['known_auxiliary_frames'], admitted_search_frames=0,
                    counts=analysis['counts'], accounting_gap=process['accounting_gap'])


if __name__ == '__main__':
    print(json.dumps(verify(), indent=2))
