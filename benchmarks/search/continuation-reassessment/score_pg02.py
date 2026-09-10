#!/usr/bin/env python3
"""Frozen conditional efficacy gates; no emulator, seed replacement or retuning."""
import json
from pathlib import Path
from run_pg01 import sha
from run_pm01 import accounting_gate


def cell_gate(q, result, campaign, usage, progress, max_drain):
    reached = result['milestone_reached_within_budget']
    frames = result['frames']
    event = result['first_milestone']
    valid_event = (event is not None and 0 < event['execution'] <= result['executions']
                   and 0 < event['frames_emulated'] <= q['frames'])
    alternatives = progress['retention_diagnostics']['alternative_admissions']
    witness = result['witness']
    # Zero activation in an optional arm is valid efficacy data, not a reason
    # to prune the seed. Ordinary retention cannot admit an alternate.
    return (isinstance(reached, bool) and reached == valid_event
            and result['complete'] is True
            and (reached or frames >= q['frames'])
            and 0 < result['executions'] <= q['executions']
            and frames <= q['frames'] + max_drain
            and result['frame_budget_overshoot'] == max(0, frames - q['frames'])
            and result['full_campaign_replay'] is True
            and result['root_local_witness_replays'] == 2
            and result['complete_prefix_witness_replays'] == 2
            and result['root']['snapshot_sha256'] == q['expected_snapshot_sha256']
            and result['root']['qualified_retention_progress'] == q['expected_retention_progress']
            and result['milestone'] == q['milestone'] == 'ridley_defeated'
            and witness['dead'] is False
            and (not reached or witness['context']['memory']['ridley_status'] & 2 != 0)
            and campaign['slot_retention'] == q['slot_retention']
            and campaign['campaign_seed'] == q['seed']
            and campaign['workers'] == q['workers'] == 4
            and campaign['execution_budget'] == q['executions']
            and campaign['frame_budget'] == q['frames']
            and campaign['action_limit'] == q['actions']
            and campaign['memory_budget_mib'] == q['memory_mib'] == 512
            and campaign['mixture_policy'] == 'alphabet_only'
            and campaign['suffix_policy'] == 'one_to_six'
            and campaign['parent_scheduler'] == 'room_cell_uniform_128_energy_progress_cheapest_v1:3,6,12,2'
            and campaign['terminal_policy'] == 'death_or_bcd_underflow_or_ending_v3'
            and campaign['executions_completed'] == progress['executions'] == result['executions']
            and campaign['frames_emulated'] == progress['frames_emulated'] == frames
            and (q['slot_retention'] is not None or alternatives == 0)
            and usage['completed_execution'] is True
            and result['cost'] == usage['cost']
            and result['cost']['admitted_search_frames'] == frames
            and result['cost']['campaign_replay_admitted_frames'] == frames
            and result['cost']['direct_physical_frames'] <= q['direct_frame_limit']
            and accounting_gate(q, result))


def restricted_cost(reached, physical_search, budget):
    return min(physical_search, budget) if reached else budget


def assess(master, cell, out):
    q = json.loads((Path(master['protocol']) / cell['request']).read_text())
    value = lambda name: json.loads((out / (name + '.json')).read_text())
    result, campaign, usage = value('result'), value('campaign'), value('usage')
    lines = (out / 'progress.jsonl').read_bytes()
    assert lines.endswith(b'\n')
    progress = json.loads(lines.splitlines()[-1])
    assert cell_gate(q, result, campaign, usage, progress, master['accepted_drain_frames'])
    assert result['stream_sha256'] == campaign['stream_sha256'] == sha(out / 'stream.jsonl')
    for name, field in [('witness-local.json', 'local_input_sha256'),
                        ('witness-full.json', 'full_input_sha256')]:
        assert sha(out / name) == result['witness'][field]
    reached = result['milestone_reached_within_budget']
    cost = result['physical_frames']
    budget = q['frames'] + (q['workers'] + 1) * 929
    return dict(valid=True, cell=cell['id'], pair=cell['pair'], arm=cell['arm'], seed=q['seed'],
                reached=reached, restricted_cost=restricted_cost(reached, cost['search_engine'], budget),
                physical_frames=cost, admitted_frames=result['frames'],
                auxiliary_frames=cost['total'] - result['frames'], executions=result['executions'],
                alternative_admissions=progress['retention_diagnostics']['alternative_admissions'],
                drain_frames=result['frame_budget_overshoot'], stop_reason=result['stop_reason'],
                archive_bytes=campaign['resident_memory_bytes'])


def panel_score(rows, pairs=4):
    if any(r.get('valid') is not True for r in rows):
        return dict(status='invalid', reason='A cell failed its frozen validity gate.')
    grouped = {}
    for row in rows:
        arms = grouped.setdefault(row['pair'], {})
        if row['arm'] in arms:
            raise ValueError('Duplicate cell')
        arms[row['arm']] = row
    assert len(grouped) <= pairs
    assert sorted(grouped) == list(range(len(grouped)))
    assert all(set(arms) == {'ordinary', 'progress', 'capacity'} for arms in grouped.values())
    assert all(len({r['seed'] for r in arms.values()}) == 1 for arms in grouped.values())
    assert len({arms['progress']['seed'] for arms in grouped.values()}) == len(grouped)
    remaining = pairs - len(grouped)
    comparisons = {}
    for comparator in ['ordinary', 'capacity']:
        candidate = [arms['progress']['restricted_cost'] for arms in grouped.values()]
        control = [arms[comparator]['restricted_cost'] for arms in grouped.values()]
        wins = sum(a < b for a, b in zip(candidate, control))
        # Exact integer arithmetic: no rounded ratio can pass the 15% gate.
        ratio_pass = bool(control) and 100 * sum(candidate) <= 85 * sum(control)
        comparisons[comparator] = dict(strict_wins=wins, completed_pairs=len(grouped),
                                       candidate_sum=sum(candidate), comparator_sum=sum(control),
                                       mean_ratio=sum(candidate) / sum(control) if control else None,
                                       ratio_pass=ratio_pass)
    futile = any(c['strict_wins'] + remaining < 3 for c in comparisons.values())
    passed = remaining == 0 and all(c['strict_wins'] >= 3 and c['ratio_pass'] for c in comparisons.values())
    status = 'passed' if passed else 'futile' if futile else 'failed' if remaining == 0 else 'continue'
    return dict(status=status, comparisons=comparisons, unrun_pairs=remaining,
                scope='Conditional endpoint only; a pass earns a separate fresh-development decision.')
