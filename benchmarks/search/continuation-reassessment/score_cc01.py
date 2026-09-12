#!/usr/bin/env python3
"""Score independent continuation confirmation without pooling development data."""
import argparse
import hashlib
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'action-correlation'))
from score_persistence import score as shared_score


def score(registration, evidence):
    reg = json.loads(registration.read_text())
    assert hashlib.sha256(Path(__file__).read_bytes()).hexdigest() == reg['panel_adapter_sha256']
    result = shared_score(registration, evidence)
    result['format'] = 'continuation-cc01-analysis-v1'
    result['decision'] = {
        'stop_persistence_line': 'stop_continuation_confirmation_line',
        'earns_transfer_and_depth_decision': 'earns_mm2_transfer_decision',
    }.get(result['decision'], result['decision'])
    result['limits'] = 'Prospective independent first-Bombs confirmation at the unchanged policy and200M horizon. CD01 development records are excluded. Allocation gate, not population significance or a boss/Wily result. Setup and unadmitted work remain additional unknowns.'
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--registration', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    assert not args.out.exists()
    args.out.write_text(json.dumps(score(args.registration, args.evidence), indent=2) + '\n')
