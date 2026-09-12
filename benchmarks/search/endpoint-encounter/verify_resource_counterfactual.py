#!/usr/bin/env python3
"""Verify default identity and the single explicit RF01 resource intervention."""
import argparse
from collections import Counter
import json
from pathlib import Path

from resource_program import canonical_sha, expected_intervention, verify_program
from score_s01 import raw, score


def read(root, name):
    return json.loads(raw(root, name))


def cell(protocol, output, name):
    registration = read(protocol, f'rf01-{name}-registration.json')
    process = read(output, name+'-process.json')
    assert process['returncode'] == 0 and process['stop_reason'] is None
    assert process['wall_seconds'] <= registration['max_wall_seconds']
    assert process['maxrss_kib'] <= 512*1024
    result = score(protocol, output/name, panel=f'rf01-{name}')
    result['process'] = process
    return result


def verify(protocol, output, phase):
    registration = read(protocol, 'rf01-registration.json')
    control = cell(protocol, output, 'control')
    for file in ('summary.json','usage.json','trials.jsonl','suffixes.json',
                 'ordinary-surviving-damage.json','root.json'):
        assert raw(output/'control',file) == raw(protocol/'s01-output',file), 'default changed: '+file
    result = {'format':'metroid-resource-counterfactual-rf01-analysis-v1',
              'default_identity_pass':True, 'cells':{'control':control},
              'decision':'default_qualified'}
    if phase == 'all':
        candidate = cell(protocol, output, 'full')
        expected = read(protocol, 'rf01-expected-snapshots.json')
        before = read(output/'full', 'resource-before-snapshot.json')
        after = read(output/'full', 'resource-root-snapshot.json')
        assert canonical_sha(before) == expected['before_canonical_sha256']
        assert after == expected_intervention(before, 1999, 20)
        assert canonical_sha(after) == expected['after_canonical_sha256']
        operation = read(output/'full', 'resource-operation.json')
        assert operation['resources'] == {'health':1999,'missiles':20}
        prefix = read(protocol, 'first-encounter-input.json')['actions']
        assert operation['prefix_actions'] == len(prefix)
        full = read(output/'full', 'summary.json')
        assert full['format'] == 'metroid-resource-counterfactual-v1'
        assert full['resource_operation'] == operation
        assert raw(output/'full','suffixes.json') == raw(output/'control','suffixes.json')
        assert raw(output/'full','root.json') == raw(output/'control','root.json')
        for record in full['witnesses']:
            witness = read(output/'full', record['file'])
            verify_program(witness['resource_operation'], operation, witness['input']['actions'], prefix)
        factual = read(output/'control','summary.json')
        pairs = {}
        for arm in ('ordinary','passive'):
            a = [r['outcome'] for r in factual['trials'] if r['arm']==arm]
            b = [r['outcome'] for r in full['trials'] if r['arm']==arm]
            pairs[arm] = dict(Counter(
                ('both' if x['surviving_defeat_endpoint'] else 'full_only') if y['surviving_defeat_endpoint']
                else ('control_only' if x['surviving_defeat_endpoint'] else 'neither')
                for x,y in zip(a,b)))
        ordinary = candidate['arms']['ordinary']['surviving_defeat_endpoints']
        passive = candidate['arms']['passive']['surviving_defeat_endpoints']
        result.update({'decision': 'resource_enabled_ordinary_defeat' if ordinary and not passive else
                                  'resource_enabled_passive_defeat_present' if passive else
                                  'no_defeat_with_full_resources_at_episode_bounds',
                       'exact_resource_state_check':True,'resource_operation':operation,
                       'paired_observed_defeats':pairs})
        result['cells']['full'] = candidate
    result['known_auxiliary_frames'] = sum(c['known_auxiliary_frames'] for c in result['cells'].values())
    assert result['known_auxiliary_frames'] <= registration['block_known_auxiliary_ceiling']
    result['scope'] = 'Joint artificial resource intervention on one original root and reused suffix bank; no fresh discovery, policy efficiency or population inference.'
    result['limits'] = ['Only the first surviving damage/defeat witness per arm is independently held-replayed twice.',
                        'Different death times imply unequal actual physical exposure; all physical frames are charged.',
                        'Released-button trials share a trajectory and are not independent replications.']
    return result


if __name__ == '__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--protocol',type=Path,default=Path(__file__).parent)
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--phase',choices=['control','all'],required=True)
    p.add_argument('--out',type=Path,required=True)
    a=p.parse_args()
    a.out.write_text(json.dumps(verify(a.protocol,a.output,a.phase),indent=2)+'\n')
