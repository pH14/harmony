#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Require a decided stale read after a RegionServer clock step."""
import json
import sys
from pathlib import Path

report = json.loads(Path(sys.argv[1]).read_text())
actions = json.loads(Path(sys.argv[2]).read_text())
assert len(actions) == 3
clock = actions[1]['operation']['Clock']
assert clock['node'] == 8 and clock['offset_ms'] == -1000
assert clock['rate_ppm'] == 0 and clock['ticks'] in (6000, 12000)
assert report['bug_found'], 'expected a recorded stale-read finding'
assert not report['never_satisfied'], report['never_satisfied']
assert report['execution_failures'] == 0
assert report['watchdog_cutoffs'] == 0
assert report['replays'], 'missing execution evidence'
for replay in report['replays']:
    assert replay['actions_applied'] == len(actions), 'scenario did not finish'
    timeline = replay['timeline']
    for i, expected in enumerate(actions, 1):
        assert timeline[i]['action']['operation'] == expected['operation'], 'wrong scenario'
    before = timeline[1]['observation']
    after = timeline[-1]['observation']
    assert before['alive'] == after['alive'] == 1023, 'a service was unavailable'
    assert after['unexpected_deaths'] == 0 and after['restarts'] == 0, 'a service restarted'
    assert before['assertions']['register: a write was acknowledged']['passed']
    assert replay['check']['pending_faults'] == 0
    assert replay['check']['undecided_keys'] == 0
    assert 'harness: a check read succeeded after a fault' in replay['check']['points']
    violations = set(replay['violations'])
    assert violations == {
        "register: each key's history is linearizable",
        'register: no read returns a value older than an acknowledged earlier write',
    }, violations
print('HBase clock-skew witness passed: acknowledged write followed by a stale read')
