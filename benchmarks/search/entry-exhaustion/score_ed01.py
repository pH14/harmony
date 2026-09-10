#!/usr/bin/env python3
"""Reuse the frozen four-pair scorer for the distinct entry-cutoff ablation."""
import argparse
import hashlib
import json
from pathlib import Path
import sys

sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'action-correlation'))
from score_persistence import score as shared_score


def score(registration,evidence):
    reg=json.loads(registration.read_text())
    assert hashlib.sha256(Path(__file__).read_bytes()).hexdigest()==reg['panel_adapter_sha256']
    result=shared_score(registration,evidence)
    result['format']='entry-exhaustion-ed01-analysis-v1'
    result['decision']={'stop_persistence_line':'stop_entry_cutoff_line',
                        'earns_transfer_and_depth_decision':'earns_independent_confirmation'}.get(result['decision'],result['decision'])
    result['limits']='Prospective cutoff3 versus64 development, scored without earlier panels. An allocation gate, not population significance or a boss/Wily result. Setup and unadmitted work remain additional unknowns.'
    return result


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--registration',type=Path,required=True)
    p.add_argument('--evidence',type=Path,required=True)
    p.add_argument('--out',type=Path,required=True)
    a=p.parse_args();a.out.write_text(json.dumps(score(a.registration,a.evidence),indent=2)+'\n')
