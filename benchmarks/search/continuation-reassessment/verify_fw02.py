#!/usr/bin/env python3
"""Recompute compatibility and exact work while preserving FW01's failed gate."""
from datetime import datetime, timezone
import json
from pathlib import Path

from run_fw01 import check_result as original_check
from run_fw02 import check_result, sha

HERE = Path(__file__).resolve().parent


def verify(root):
    reg = json.loads((root / 'fw02-registration.json').read_text())
    for path, expected in reg['protocol_sha256'].items():
        assert sha(root / path) == expected, path
    old = json.loads((root / 'fw01-output/results.json').read_text())
    assert not old['complete'] and len(old['records']) == 1
    assert old['records'][0]['exit_code'] == 0 and not old['records'][0]['verified']
    reused = reg['reused_fw01_case']
    try:
        original_check(reg, reused, root / reused['output'])
    except AssertionError:
        pass
    else:
        raise AssertionError('The frozen FW01 failure was silently changed')
    reused_result = check_result(reg, reused, root / reused['output'])
    assert reused_result['known_auxiliary_frames'] == old['records'][0]['known_auxiliary_frames'] == 234966
    output = root / 'fw02-output'
    panel = json.loads((output / 'results.json').read_text())
    assert panel['registration_sha256'] == sha(root / 'fw02-registration.json')
    assert panel['registration_commit'] == 'b7b2ec8957acf2db2e314cfc9b7fd1cc6cf40bf1'
    assert panel['complete'] and [r['id'] for r in panel['records']] == [c['id'] for c in reg['cases']]
    total = 0
    for case, record in zip(reg['cases'], panel['records']):
        assert record['verified'] and record['exit_code'] == 0 and record['stop_reason'] is None
        actual = check_result(reg, case, output / case['id'])
        assert all(record[key] == value for key, value in actual.items())
        assert record['budget_charged_upper_frames'] == actual['known_auxiliary_frames']
        assert record['elapsed_seconds'] <= reg['case_wall_seconds']
        total += actual['known_auxiliary_frames']
    assert total == reg['expected_auxiliary_frames'] <= reg['auxiliary_frame_ceiling']
    journal = [json.loads(line) for line in (output / 'service-journal.jsonl').read_text().splitlines()]
    invocation = '198f9d602e144ae7b0e99b33d7ac2ccc'
    assert all(j.get('INVOCATION_ID', j.get('_SYSTEMD_INVOCATION_ID')) == invocation for j in journal)
    emitted = [json.loads(j['MESSAGE']) for j in journal if j['MESSAGE'].startswith('{')]
    assert emitted == panel['records']
    started = next(j for j in journal if j['MESSAGE'].startswith('Started '))
    stopped = next(j for j in journal if j['MESSAGE'].endswith('Deactivated successfully.'))
    accounting = next(j for j in journal if 'CPU_USAGE_NSEC' in j)
    elapsed = (int(stopped['__REALTIME_TIMESTAMP']) - int(started['__REALTIME_TIMESTAMP'])) / 1e6
    assert elapsed <= reg['block_wall_seconds']
    assert int(accounting['MEMORY_PEAK']) <= 4 * 1024**3
    assert datetime.fromtimestamp(int(stopped['__REALTIME_TIMESTAMP']) / 1e6, timezone.utc) < datetime.fromisoformat(reg['deadline_utc'])
    native_files = [p for p in output.rglob('*') if p.is_file() and p.name != 'service-journal.jsonl']
    assert all(p.stat().st_size <= reg['file_limit_bytes'] for p in native_files)
    assert sum(p.stat().st_size for p in native_files) <= reg['output_limit_bytes']
    prior = json.loads((root / 'ledger-after-fw01.json').read_text())
    assert sha(root / 'ledger-after-fw01.json') == reg['prior_ledger_sha256']
    return {'format': 'continuation-fw02-closed-ledger-v1',
            'decision': 'all_six_historical_witnesses_compatible_under_source_corrected_contract',
            'fw01_original_gate': 'failed; unchanged', 'fw02_result': 'pass',
            'registration_sha256': sha(root / 'fw02-registration.json'),
            'results_sha256': sha(output / 'results.json'),
            'prior_ledger_sha256': reg['prior_ledger_sha256'],
            'admitted_search_frames': 0, 'known_auxiliary_frames': total,
            'budget_charged_upper_frames': total, 'reused_frames_not_charged_again': 234966,
            'combined_fw01_fw02_known_frames': total + 234966,
            'service_elapsed_seconds': elapsed, 'service_cpu_nanoseconds': int(accounting['CPU_USAGE_NSEC']),
            'service_memory_peak_bytes': int(accounting['MEMORY_PEAK']),
            'service_status': 'deactivated successfully; transient unit unloaded',
            'partial_emulator_advancement': 'none: all new three-pass replays completed',
            'cumulative_since_user_resumption_admitted_search_frames': prior['cumulative_since_user_resumption_admitted_search_frames'],
            'cumulative_since_user_resumption_known_auxiliary_frames': prior['cumulative_since_user_resumption_known_auxiliary_frames'] + total,
            'limits': ['Witness compatibility only; no fresh corrected-terminal performance or boss evidence.',
                       'Historical performance used terminal v2/key v8; current source uses terminal v3/key v10.',
                       'Prior failed gates, expired-tranche totals and unknown work gaps remain unchanged.']}


if __name__ == '__main__':
    print(json.dumps(verify(HERE), indent=2))
