#!/usr/bin/env python3
"""Verify the closed RR01 failure without importing or executing an emulator."""
import gzip
import hashlib
import json
from pathlib import Path
import subprocess


HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
SOURCE_COMMIT = 'eb6f0dcc7d87a12ae5b9a1fceccef1bfde7b97cf'
SOURCE_HASHES = {
    'workloads/nes-machine/src/quicknes.rs': 'ff5fe16b3810bae0af23150fba61f20983a8c00d03ac71a93c0871bfbddd6ece',
    'workloads/nes/src/metroid/target.rs': 'cf63ea5f364b16ade50cda082e4f408a577627909f2d343895909ff363810141',
    'workloads/nes/src/bin/metroid-retention-replay.rs': '2381e9a855b2f06baf0a56b6cf6910f8ff02d62b2fd2ec095ef734d6155ec7bc',
}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read_json(path):
    return json.loads(path.read_bytes())


def verify():
    output = HERE / 'rr01-output'
    master = read_json(HERE / 'rr01-registration.json')
    process = read_json(output / 'rr01-process.json')
    request = read_json(HERE / 'rr01-qualify-request.json')
    assert process['registration_sha256'] == digest((HERE / 'rr01-registration.json').read_bytes())
    for name, expected in master['protocol_sha256'].items():
        assert digest((HERE / name).read_bytes()) == expected, name
    for name, expected in master['scorer_dependency_sha256'].items():
        assert digest((REPO / name).read_bytes()) == expected, name
    for name, expected in process['files'].items():
        assert digest((output / name).read_bytes()) == expected, name
    assert process['hostname'] == 'ms02'
    assert process['cpus'] == master['cpus']
    assert process['cgroup_limits'] == master['cgroup_limits']
    assert process['decision'] == 'qualification_failed_inspection_unrun'
    assert process['admitted_search_frames'] == 0
    # The controller sums completed reports only. This zero is not total work.
    assert process['known_auxiliary_frames'] == 0
    assert len(process['phases']) == 1
    phase = process['phases'][0]
    assert phase == read_json(output / 'qualify-process.json')
    assert phase['phase'] == 'qualify' and phase['exit_code'] == 1
    assert phase['killed'] is None and 'result' not in phase
    assert not (output / 'inspect').exists()
    assert not (output / 'inspect-request.json').exists()
    assert not (output / 'qualify/result.json').exists()
    failure = read_json(output / 'qualify/failure.json')
    assert failure['status'] == 'incomplete'
    assert failure['error'] == 'machine backend failed: snapshot is not compatible with this QuickNES revision/build'
    assert failure['executable_sha256'] == master['binary_sha256']
    assert failure['request_sha256'] == digest((HERE / 'rr01-qualify-request.json').read_bytes())

    original = {}
    ap01 = HERE.parent / 'endpoint-encounter/ap01-output'
    for item in master['saved_inputs']:
        raw = gzip.decompress((ap01 / (item['name'] + '.gz')).read_bytes())
        assert len(raw) == item['bytes'] and digest(raw) == item['sha256']
        original[item['name']] = raw
    root = json.loads(gzip.decompress((ap01 / 'root-snapshot.json.gz').read_bytes()))
    emulator = bytes(root['emulator_state'])
    assert original['origin.bin'].count(emulator) == 1
    assert emulator[:8] == b'HQNESST2'
    assert emulator[8:48] == b'26bb785c9deddb66a17717b21bb4e328f03ade32'
    stored_core = emulator[48:112].decode('ascii')
    assert stored_core == request['original_core_sha256'] != request['core']['sha256']
    assert len(emulator) == 120 + int.from_bytes(emulator[112:120], 'little')
    assert original['checkpoint.bin'].count(stored_core.encode()) == 588
    before, body = original['stream.jsonl'].split(b'\n', 1)
    after, selected = (output / 'qualify/derived-stream.jsonl').read_bytes().split(b'\n', 1)
    before, after = json.loads(before), json.loads(after)
    assert [key for key in before if before[key] != after[key]] == ['emulator_backend']
    assert before.keys() == after.keys()
    assert after['emulator_backend'] == before['emulator_backend'].replace(stored_core, request['core']['sha256'])
    assert before['resume_input_sha256'] == digest(b'{"actions":[]}')
    assert selected == b''.join(body.splitlines(keepends=True)[:4])
    assert sum(json.loads(line)['frames'] for line in selected.splitlines()) == 865
    for name, expected in SOURCE_HASHES.items():
        raw = subprocess.check_output(['git', 'show', SOURCE_COMMIT + ':' + name], cwd=REPO)
        assert digest(raw) == expected, name

    journal = [json.loads(line) for line in (output / 'service-journal.jsonl').read_text().splitlines()]
    invocation = process['invocation_id']
    assert all(row.get('INVOCATION_ID', row.get('_SYSTEMD_INVOCATION_ID')) == invocation for row in journal)
    assert any(row.get('EXIT_STATUS') == '1' for row in journal)
    assert any(row.get('UNIT_RESULT') == 'exit-code' for row in journal)
    resources = [row for row in journal if 'CPU_USAGE_NSEC' in row]
    assert len(resources) == 1
    state = dict(line.split('=', 1) for line in (output / 'service-state.txt').read_text().splitlines())
    assert state == dict(MainPID='0', ActiveState='inactive', SubState='dead')
    return dict(
        format='retention-replay-rr01-failure-analysis-v1',
        decision=process['decision'],
        registration_sha256=process['registration_sha256'],
        process_sha256=digest((output / 'rr01-process.json').read_bytes()),
        source_commit=SOURCE_COMMIT, source_sha256=SOURCE_HASHES,
        failure=failure['error'],
        cause='Original raw snapshot embeds the ARM core hash; the exact-build import guard rejects the ms02 core before unserialize.',
        root_emulator_bytes=len(emulator), original_core_sha256=stored_core,
        runtime_core_sha256=request['core']['sha256'],
        replay_jobs_executed=0, inspection='unrun; local-loss query unmeasured',
        admitted_search_frames=0,
        runtime_receipted_auxiliary_frames=0,
        source_inferred_inspector_setup_frames=929,
        accounting='929 is inferred from the pinned caller reaching its first foreign-root restore after the setup-clock equality check. The failure report did not persist that counter; do not call this a runtime frame receipt. No replay target is constructed on this path. Historical unmeasured costs remain unknown.',
        phase_wall_seconds=phase['wall_seconds'], phase_cpu_seconds=phase['cpu_seconds'],
        phase_peak_rss_kib=phase['peak_rss_kib'],
        service_cpu_nanoseconds=int(resources[0]['CPU_USAGE_NSEC']),
        service_memory_peak_bytes=int(resources[0]['MEMORY_PEAK']),
        service_invocation=invocation, service_state=state,
        next_decision='RR01 closed without retry. Same-build raw replay is the supported contract. Any cross-build diagnostic must regenerate from the exact searched action prefix under a distinct prospective protocol; no policy or native allocation is earned here.')


if __name__ == '__main__':
    print(json.dumps(verify(), indent=2))
