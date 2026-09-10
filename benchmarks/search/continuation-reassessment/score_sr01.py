#!/usr/bin/env python3
"""SR01 implementation qualification only; no efficacy claim or retries."""
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
            and campaign_identity(q, result, campaign, progress)
            and (q['slot_retention'] is not None or alternatives == 0)
            and usage['completed_execution'] is True
            and result['cost'] == usage['cost']
            and result['cost']['admitted_search_frames'] == frames
            and result['cost']['campaign_replay_admitted_frames'] == frames
            and result['cost']['direct_physical_frames'] <= q['direct_frame_limit']
            and accounting_gate(q, result))



def campaign_identity(q, result, campaign, progress):
    return (campaign.get('slot_retention') == q.get('slot_retention')
            and campaign['campaign_seed'] == q['seed']
            and campaign['workers'] == q['workers'] == 4
            and campaign['execution_budget'] == q['executions']
            and campaign['frame_budget'] == q['frames']
            and campaign['action_limit'] == q['actions']
            and campaign['memory_budget_mib'] == q['memory_mib'] == 512
            and campaign['mixture_policy'] == 'alphabet_only'
            and campaign['suffix_policy'] == 'one_to_six'
            and campaign['parent_scheduler'] == q.get('selector', 'room_cell_uniform_128_energy_progress_cheapest_v1:3,6,12,2')
            and campaign['terminal_policy'] == 'death_or_bcd_underflow_or_ending_v3'
            and campaign['executions_completed'] == progress['executions'] == result['executions']
            and campaign['frames_emulated'] == progress['frames_emulated'] == result['frames'])

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
    return dict(valid=True, cell=cell['id'], pair=cell['pair'], arm=cell['arm'], seed=q['seed'],
                reached=reached,
                physical_frames=cost, admitted_frames=result['frames'],
                auxiliary_frames=cost['total'] - result['frames'], executions=result['executions'],
                alternative_admissions=progress['retention_diagnostics']['alternative_admissions'],
                drain_frames=result['frame_budget_overshoot'], stop_reason=result['stop_reason'],
                archive_bytes=campaign['resident_memory_bytes'])



def first_return(control, candidate):
    """First divergence must be a changed parent from an identical job history."""
    with Path(control).open() as left, Path(candidate).open() as right:
        a, b = json.loads(next(left)), json.loads(next(right))
        expected = "room_cell_uniform_128_energy_progress_cheapest_scoped_return_"
        assert a.pop('parent_scheduler') == expected + 'control_v1:3,6,12,2'
        assert b.pop('parent_scheduler') == expected + 'half_v1:3,6,12,2'
        assert a == b
        matched = 0
        for l, r in zip(left, right):
            a, b = json.loads(l), json.loads(r)
            if a == b:
                matched += 1
                continue
            activated = (a.get('event') == b.get('event') == 'job'
                         and a['parent_id'] != b['parent_id']
                         and all(a[k] == b[k] for k in ['sequence', 'worker', 'mutation_seed', 'selector'])
                         and a['selector']['path'] == 'room_cell_uniform')
            return dict(activated=activated, identical_prior_records=matched,
                        first_control=a, first_candidate=b)
    return dict(activated=False, identical_prior_records=matched, reason='No parent divergence')
