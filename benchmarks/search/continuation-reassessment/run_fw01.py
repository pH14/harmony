#!/usr/bin/env python3
"""Run the frozen six-witness compatibility block, stopping on the first failure."""
import argparse
from datetime import datetime, timezone
import gzip
import hashlib
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import time


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check_result(reg, case, folder):
    summary = json.loads((folder / 'summary.json').read_text())
    assert summary['format'] == 'metroid-boss-memory-probe-v1'
    assert summary['verified_replays'] == 3
    assert summary['terminal_policy'] == 'death_or_bcd_underflow_or_ending_v3'
    for key in ('rom_sha256', 'core_sha256'):
        assert summary[key] == reg[key]
    assert summary['input_sha256'] == case['input_sha256']
    for name, field in [('ordinary.json', 'ordinary'), ('first.json', 'one_frame')]:
        assert json.loads((folder / name).read_text()) == summary[field]
    for field in ('endpoint', 'raw_endpoint', 'emulator_sha256', 'route_frames'):
        assert summary['ordinary'][field] == summary['one_frame'][field]
    for mode in ('ordinary', 'one_frame'):
        result = summary[mode]
        assert result['route_frames'] == case['route_frames']
        assert result['setup_frames'] == 929
        assert result['physical_frames_including_setup'] == case['route_frames'] + 929
        state = result['endpoint']
        assert state['mode'] == 3 and 0 < state['health'] < 8000 and not state['ending']
        assert state['equipment'] & 0x01  # NamedProgress::GEAR: Bombs, no route guidance.
    assert (folder / 'relevant-frames.jsonl').stat().st_size == summary['one_frame']['relevant_trace_bytes']
    frames = (summary['ordinary']['physical_frames_including_setup']
              + 2 * summary['one_frame']['physical_frames_including_setup'])
    assert frames == case['expected_three_pass_frames']
    return {'known_auxiliary_frames': frames,
            'endpoint': summary['one_frame']['endpoint'],
            'summary_sha256': sha(folder / 'summary.json')}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--protocol', type=Path, required=True)
    parser.add_argument('--preflight-only', action='store_true')
    args = parser.parse_args()
    reg_path = args.protocol / 'fw01-registration.json'
    reg = json.loads(reg_path.read_text())
    for name, expected in reg['protocol_sha256'].items():
        assert sha(args.protocol / name) == expected, name
    binary = args.root / reg['binary']
    assert sha(binary) == reg['binary_sha256']
    assert sha(binary.with_name('build-info.json')) == reg['build_info_sha256']
    assets = json.loads((args.root / 'assets.json').read_text())
    core, rom = (Path(assets[key]['path']) for key in ('core', 'metroid'))
    assert sha(core) == reg['core_sha256'] and sha(rom) == reg['rom_sha256']
    total = 0
    for case in reg['cases']:
        input_path = args.protocol / case['input']
        assert sha(input_path) == case['input_sha256']
        actions = json.loads(input_path.read_text())['actions']
        assert len(actions) == case['actions'] <= 8192
        assert all(1 <= action['hold_frames'] <= 128 for action in actions)
        frames = sum(action['hold_frames'] for action in actions)
        assert frames == case['route_frames'] <= 250000
        assert 3 * (frames + 929) == case['expected_three_pass_frames']
        producing_bytes = gzip.decompress((args.protocol / case['producing_summary']).read_bytes())
        assert hashlib.sha256(producing_bytes).hexdigest() == case['producing_summary_sha256']
        producing = json.loads(producing_bytes)
        assert producing['result']['milestone_witnesses']['bombs']['input_sha256'] == case['input_sha256']
        total += case['expected_three_pass_frames']
    assert total == reg['expected_auxiliary_frames'] <= reg['auxiliary_frame_ceiling']
    assert (datetime.fromisoformat(reg['deadline_utc']) - datetime.now(timezone.utc)).total_seconds() >= reg['block_wall_seconds']
    output = args.root / reg['output']
    assert not output.exists()
    if args.preflight_only:
        print(json.dumps({'preflight': 'pass', 'expected_auxiliary_frames': total}), flush=True)
        return
    registration_commit = os.environ.get('HARMONY_REGISTRATION_COMMIT', '')
    assert len(registration_commit) == 40 and all(c in '0123456789abcdef' for c in registration_commit)
    output.mkdir(parents=True)
    resource.setrlimit(resource.RLIMIT_FSIZE, (reg['file_limit_bytes'], reg['file_limit_bytes']))
    records = []
    for case in reg['cases']:
        assert (datetime.fromisoformat(reg['deadline_utc']) - datetime.now(timezone.utc)).total_seconds() >= reg['case_wall_seconds']
        folder = output / case['id']
        start = time.monotonic()
        reason = None
        with (output / (case['id'] + '.log')).open('wb') as log:
            child = subprocess.Popen(['taskset', '-c', reg['cpu'], str(binary), str(core), str(rom),
                                      str(args.protocol / case['input']), str(folder)],
                                     stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            while child.poll() is None:
                if time.monotonic() - start > reg['case_wall_seconds']:
                    reason = 'wall_limit'
                elif sum(p.stat().st_size for p in output.rglob('*') if p.is_file()) > reg['output_limit_bytes']:
                    reason = 'output_limit'
                if reason:
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    break
                time.sleep(0.2)
            code = child.wait()
        usage = resource.getrusage(resource.RUSAGE_CHILDREN)
        record = {'id': case['id'], 'exit_code': code, 'stop_reason': reason,
                  'elapsed_seconds': time.monotonic() - start,
                  'cumulative_child_cpu_seconds': usage.ru_utime + usage.ru_stime,
                  'cumulative_child_maxrss_kib': usage.ru_maxrss,
                  'budget_charged_upper_frames': case['expected_three_pass_frames'], 'verified': False}
        if code == 0 and reason is None:
            try:
                record.update(check_result(reg, case, folder), verified=True)
            except (AssertionError, KeyError, ValueError, OSError) as error:
                record['verification_error'] = type(error).__name__ + ': ' + str(error)
        if not record['verified']:
            known = 0
            for name in ('ordinary.json', 'first.json'):
                try:
                    known += json.loads((folder / name).read_text())['physical_frames_including_setup']
                except (OSError, ValueError, KeyError):
                    pass
            try:
                completed = json.loads((folder / 'summary.json').read_text())
                if completed['verified_replays'] == 3:
                    known = (completed['ordinary']['physical_frames_including_setup']
                             + 2 * completed['one_frame']['physical_frames_including_setup'])
                    record['partial_advancement'] = 'all three native passes completed; compatibility verification failed'
            except (OSError, ValueError, KeyError):
                pass
            record['known_auxiliary_frames'] = known
            record.setdefault('partial_advancement', 'unknown; full registered case bound remains charged')
        records.append(record)
        report = {'format': 'continuation-fw01-results-v1', 'registration_sha256': sha(reg_path),
                  'registration_commit': registration_commit,
                  'records': records, 'complete': len(records) == len(reg['cases']) and all(r['verified'] for r in records)}
        (output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(record), flush=True)
        if not record['verified']:
            raise SystemExit(1)


if __name__ == '__main__':
    main()
