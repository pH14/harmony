#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Require an exercised HBase fault and a successful final recovery check."""
import json
import sys
from pathlib import Path

report = json.loads(Path(sys.argv[1]).read_text())
actions = json.loads(Path(sys.argv[2]).read_text())
faults = [i for i, action in enumerate(actions) if 'Wait' not in action['operation']]
assert faults and faults[0] > 0, 'require a warmup before the fault'
assert not report['bug_found'], 'recorded finding'
assert not report['never_satisfied'], report['never_satisfied']
assert report['execution_failures'] == 0
assert report['watchdog_cutoffs'] == 0
assert report['replays'], 'missing execution evidence'
for replay in report['replays']:
    assert not replay['violations'], replay['violations']
    assert replay['actions_applied'] == len(actions), 'scenario did not finish'
    timeline = replay['timeline']
    assert len(timeline) > len(actions), 'missing action observations'
    for i, expected in enumerate(actions, 1):
        assert timeline[i]['action']['operation'] == expected['operation'], 'wrong scenario'
    before = timeline[faults[0]]['observation']
    assert before['alive'] == 1023, 'all ten services must be alive before the fault'
    assert before['assertions']['register: a write was acknowledged']['passed'], 'no acknowledged pre-fault write'
    after = timeline[-1]['observation']
    assert after['alive'] == 1023, 'not all services returned'
    assert after['unexpected_deaths'] == 0, 'unexpected service exit'
    kills = sum('Kill' in action['operation'] for action in actions)
    assert after['restarts'] >= kills, 'requested service restart was not observed'
    hooks = sum('Hook' in action['operation'] for action in actions)
    assert after['hooks_finished'] >= hooks, 'requested hook did not finish'
    check = replay['check']
    assert check['pending_faults'] == 0, 'fault remains pending'
    assert check['undecided_keys'] == 0, 'final check is undecided'
    assert 'harness: a check read succeeded after a fault' in check['points'], 'final check has no successful post-fault read'
print('HBase recovery check passed: pre-fault write, completed fault, final read')
