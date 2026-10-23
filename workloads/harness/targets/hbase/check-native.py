#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Require successful reads, writes and CAS in a retained native baseline."""
import json
import sys
from collections import Counter
from pathlib import Path

lines = Path(sys.argv[1]).read_text().splitlines()
report = json.loads(next(line for line in reversed(lines) if line.startswith('{')))
assert report['result'] == 'clean', report
assert report['last_check']['decided'], report
assert report['counts']['check_failures'] == 0, report
assert report['counts']['natural_deaths'] == 0, report
counts = Counter()
for journal in (Path(report['output']) / 'tmp/harness/journal').glob('*.jsonl'):
    for line in journal.read_text().splitlines():
        event = json.loads(line)
        counts[event['f'], event['t']] += 1
for operation in ('read', 'write', 'cas'):
    assert counts[operation, 'ok'] > 0, (operation, counts)
print(json.dumps({f'{operation}:{outcome}': count
                  for (operation, outcome), count in sorted(counts.items())}))
