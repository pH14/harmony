#!/usr/bin/env python3
"""Describe fixed cached-state cohorts and their actual historical parent draws."""
from collections import Counter, defaultdict
import gzip
import json
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'depth-transfer'))
from boss_interval import classify

ROOT = Path(__file__).resolve().parent
ROOT_SCOPE = [20, 0, 9, 64, 17, 1, 20, 0]


def qualified(row):
    state, context = row['state'], row['context']
    memory = context['memory']
    if row['dead'] or row['failed'] or state['ending']:
        return None, 'terminal'
    if [memory[k] for k in ['area', 'mode', 'door']] != [state[k] for k in ['area', 'mode', 'door']]:
        raise ValueError('Snapshot and context boundaries differ')
    if (memory['area'] == 18 and memory['kraid_status'] & 1
            or memory['area'] == 20 and memory['ridley_status'] & 2):
        return None, 'defeated'
    flat = dict(area=memory['area'], mode=memory['mode'])
    for i, enemy in enumerate(memory['enemies']):
        for dst, src in [('offset', 'slot'), ('status', 'status'), ('type', 'data_index'), ('special', 'special'), ('hp', 'hit_points')]:
            flat[f'slot{i}_{dst}'] = enemy[src]
        flat[f'slot{i}_saved_status'] = context['saved_status'][i]
    slots = [classify(flat, i) for i in range(6)]
    mask = sum(1 << i for i, slot in enumerate(slots) if slot is not None)
    assert mask == row['endpoint_boss_slots']
    slots = [slot for slot in slots if slot is not None]
    if len(slots) != 1:
        return None, 'unclassified_or_ambiguous'
    boss = slots[0]
    if boss.hp == 255:
        return None, 'hp_unavailable'
    scope = [boss.area, boss.offset, boss.data_index, boss.attributes,
             state['equipment'], state['energy_tanks'], state['missile_capacity'], state['bosses']]
    if scope != ROOT_SCOPE:
        return None, 'other_scope'
    return boss.hp, 'root_scope'


def histories(stream):
    birth, draws = {0: 0}, defaultdict(list)
    header = json.loads(next(stream))
    frames = 0
    sequence = 0
    for line in stream:
        job = json.loads(line)
        if job['event'] == 'skip':
            continue
        assert job['event'] == 'job'
        sequence += 1
        assert job['sequence'] == sequence and job['parent_id'] in birth
        frames += job['frames']
        draws[job['parent_id']].append(sequence)
        for decision in job['decisions']:
            if decision['decision'] == 'retained':
                assert decision['id'] == len(birth)
                birth[decision['id']] = sequence
    return header, birth, draws, sequence, frames


def cohort(rows):
    return dict(states=len(rows), selected_states=sum(r['parent_draws'] > 0 for r in rows),
                parent_draws=sum(r['parent_draws'] for r in rows),
                unselected_states=sum(r['parent_draws'] == 0 for r in rows),
                hp_counts=dict(sorted(Counter(str(r['hp']) for r in rows).items(), key=lambda item: int(item[0]))),
                health_counts=dict(sorted(Counter(str(r['health']) for r in rows).items(), key=lambda item: int(item[0]))),
                missile_counts=dict(Counter(str(r['missiles']) for r in rows)),
                rows=rows)


def analyze(output):
    result = {}
    for arm in ['ordinary', 'progress']:
        report = json.loads((output / (arm + '.json')).read_text())
        path = ROOT / ('pg02-output/p0-' + arm + '/campaign/stream.jsonl.gz')
        with gzip.open(path, 'rt') as stream:
            header, birth, draws, jobs, frames = histories(iter(stream))
        assert header['campaign_seed'] == 1421509093 and header['frame_budget'] == 1000000
        states = []
        excluded = Counter()
        for row in report['entries']:
            hp, reason = qualified(row)
            assert row['id'] in birth
            if hp is None:
                excluded[reason] += 1
                continue
            visits = draws[row['id']]
            assert all(sequence > birth[row['id']] for sequence in visits)
            states.append(dict(id=row['id'], hp=hp, health=row['state']['health'],
                               missiles=row['state']['missiles'], created_job=birth[row['id']],
                               jobs_after_creation=jobs - birth[row['id']], parent_draws=len(visits),
                               first_parent_draw=visits[0] if visits else None,
                               last_parent_draw=visits[-1] if visits else None,
                               retention_group=row['retention_group']))
        damaged = [row for row in states if row['hp'] < 140]
        result[arm] = dict(jobs=jobs, frames=frames, total_cached=len(report['entries']),
                           excluded=dict(excluded), root_scope=cohort(states),
                           lower_hp=cohort(damaged), equal_root_resources_lower_hp=cohort([
                               row for row in damaged if row['health'] >= 79 and row['missiles'] >= 0]))
    return dict(format='retention-pq01-cached-state-history-v1', arms=result,
                scope='All cached endpoints, including reconstruction history; not an active-state census. Historical visits are observed, opportunity and policy counterfactuals are unmeasured. Final-cache conditioning is selection bias. HP is snapshot-local, not lifetime damage or an efficacy endpoint.')


if __name__ == '__main__':
    print(json.dumps(analyze(Path(sys.argv[1])), indent=2))
