#!/usr/bin/env python3
"""Score the preregistered RC01 supplied-root comparison without emulation."""
import argparse
import json
from pathlib import Path

from score_archive_pilot import read, score, sha
from score_s01 import raw


def assess(pairs):
    assert 0 < len(pairs) <= 4
    wins = sum(p['candidate']['restricted_interval'][1] < p['control']['restricted_interval'][0]
               for p in pairs)
    if any(not c['fixed_horizon_completed'] for p in pairs for c in p.values()):
        decision = 'stop_incomplete_measurement'
    elif wins + 4 - len(pairs) < 3:
        decision = 'stop_futility'
    elif len(pairs) < 4:
        decision = 'continue'
    else:
        ratio = sum(p['candidate']['restricted_interval'][1] for p in pairs) / sum(
            p['control']['restricted_interval'][0] for p in pairs)
        resource_ok = all(sum(p['candidate']['process'][k] for p in pairs) <=
                          1.25 * sum(p['control']['process'][k] for p in pairs)
                          for k in ('wall_seconds', 'cpu_seconds'))
        decision = 'pass_conditional_gate' if wins >= 3 and ratio <= 0.85 and resource_ok else 'stop_failed_gate'
    return {'decision': decision, 'strict_wins': wins, 'completed_pairs': len(pairs),
            'unrun_pairs': 4-len(pairs),
            'restricted_cost_ratio_upper': sum(p['candidate']['restricted_interval'][1] for p in pairs) /
                sum(p['control']['restricted_interval'][0] for p in pairs)}


def cell(protocol, output, name, registration):
    q = read(protocol, name+'.json')
    spec = next(c for c in registration['cells'] if c['id'] == name)
    assert sha(raw(protocol, name+'.json')) == spec['request_sha256']
    p = read(output, name+'-process.json')
    assert p['returncode'] == 0 and p['stop_reason'] is None
    assert p['wall_seconds'] <= spec['max_wall_seconds']
    assert p['maxrss_kib'] <= 2*1024*1024
    result = score(protocol, output/name, name+'.json', 'rc01-registration.json')
    campaign = read(output/name, 'campaign.json')
    report = read(output/name, 'result.json')
    assert report['root']['endpoint'] == q['expected_endpoint']
    assert report['root']['context'] == q['expected_context']
    assert report['root']['emulator_sha256'] == q['expected_emulator_sha256']
    assert campaign['campaign_seed'] == q['seed']
    local = read(output/name, 'witness-local.json')
    retries = campaign.get('local_terminal_retries', 0)
    if q.get('local_terminal_retry', False):
        assert campaign['local_terminal_retry'] == 'one_per_live_boundary_predrawn_attempts_v1'
        assert retries > 0 and campaign.get('first_local_retry') is not None
        if not result['milestone_reached']:
            assert campaign['first_local_retry']['input'] == local
            assert not report['witness']['dead']
    else:
        assert retries == 0 and campaign.get('first_local_retry') is None
        assert 'local_terminal_retry' not in campaign
    rows = [json.loads(l) for l in raw(output/name, 'stream.jsonl').splitlines()]
    jobs = [r for r in rows if r.get('event') == 'job']
    assert len(jobs) == result['executions']
    assert [r['sequence'] for r in jobs] == list(range(1, len(jobs)+1))
    assert sum(r['frames'] for r in jobs) == result['admitted_frames']
    interval = [q['frames'], q['frames']]
    if result['milestone_reached']:
        first = result['observed_first_milestone']
        job = jobs[first['execution']-1]
        assert sum(r['frames'] for r in jobs[:first['execution']]) == first['frames_emulated']
        interval = [first['frames_emulated']-job['frames']+1, first['frames_emulated']]
    result['restricted_interval'] = interval
    result['retry_attempts'] = retries
    result['process'] = {**p, 'cpu_seconds': p['user_seconds']+p['system_seconds']}
    return result


def panel(protocol, output, completed_pairs):
    registration = read(protocol, 'rc01-registration.json')
    pairs = []
    for i in range(1, completed_pairs+1):
        names = {arm: f'rc01-p{i}-{arm}' for arm in ('control', 'candidate')}
        requests = {arm: read(protocol, name+'.json') for arm, name in names.items()}
        candidate = dict(requests['candidate'])
        assert candidate.pop('local_terminal_retry') is True
        assert candidate == requests['control']
        pairs.append({arm: cell(protocol, output, name, registration) for arm, name in names.items()})
        if i < completed_pairs:
            assert assess(pairs)['decision'] == 'continue', 'dispatch continued after a stopping gate'
    result = {'format': 'metroid-local-retry-rc01-analysis-v1',
              'registration_sha256': sha(raw(protocol, 'rc01-registration.json')),
              **assess(pairs), 'pairs': pairs,
              'known_auxiliary_frames': sum(c['known_auxiliary_frames'] for p in pairs for c in p.values()),
              'scope': 'New paired conditional seeds from the supplied original E01 root. Not fresh discovery or exact total-physical-work equality.',
              'unknown': 'Engine setup, unadmitted work and reconstruction remain unknown; admitted attempted frames, direct helper work and process costs are recorded.'}
    assert result['known_auxiliary_frames'] <= registration['block_known_auxiliary_ceiling']
    return result


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--protocol', type=Path, default=Path(__file__).parent)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--completed-pairs', type=int, required=True)
    p.add_argument('--out', type=Path, required=True)
    a = p.parse_args()
    a.out.write_text(json.dumps(panel(a.protocol, a.output, a.completed_pairs), indent=2)+'\n')
