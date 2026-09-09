"""Restrict an explicitly registered milestone's admitted cost to its horizon."""
import math


def validate_named_panel(registration):
    """Refuse ambiguous endpoint requests before any cell is dispatched."""
    condition = registration.get('milestone_stop')
    if condition is not None:
        if (not isinstance(condition, dict) or set(condition) != {'name', 'observation_policy'}
                or any(not isinstance(value, str) or not value for value in condition.values())):
            raise ValueError('milestone_stop must name and version one observation')
    if 'screen' in registration and registration['screen'].get('milestone_stop') != condition:
        raise ValueError('screen and panel must register the same milestone stop')
    if condition is not None and 'screen' in registration:
        resources = registration['screen'].get('resources') or {}
        ratio = resources.get('max_candidate_to_control_ratio')
        if (resources.get('measurement') != 'event_stopped_cell_totals_v1'
                or type(ratio) not in (int, float) or not math.isfinite(ratio) or ratio <= 0):
            raise ValueError('named milestone screens require a finite event-stopped resource gate')
    for cell in registration['cells']:
        for case in cell['manifest']['cases']:
            search = {**cell['manifest']['search'], **case.get('search', {})}
            requested = search.get('stop_after_milestone')
            expected = condition['name'] if condition else None
            if requested != expected:
                raise ValueError('cell and panel must register the same milestone stop')
            if condition is not None and search.get('frames') != cell['manifest']['search'].get('frames'):
                raise ValueError('case overrides cannot change the registered milestone horizon')
    return condition


def named_milestone_endpoint(summary, budget, condition):
    """Completed evaluator output is verified upstream; this checks its metric contract.

    Early nonattainment remains incomplete evidence. A drained observation beyond
    the frame horizon is retained but restricted to that horizon as a non-hit.
    The timestamp is a complete admitted-job cost, not an exact pickup frame.
    """
    def count(value, name):
        if type(value) is not int or value < 0:
            raise ValueError(name + ' must be a nonnegative integer')
        return value

    count(budget, 'budget')
    if budget == 0:
        raise ValueError('budget must be positive')
    request = summary.get('search_request') or {}
    identity = summary.get('identity') or {}
    result = summary.get('result') or {}
    if summary.get('status') != 'complete' or result.get('status') != 'complete':
        raise ValueError('completed verified evaluator output is required')
    if request.get('frames') != budget or identity.get('frames') != budget:
        raise ValueError('requested and reported horizons must match the registration')
    if (request.get('stop_after_milestone') != condition['name']
            or identity.get('milestone_stop') != condition
            or result.get('milestone_stop') != condition):
        raise ValueError('requested, recorded and registered milestone meanings differ')
    if result.get('verification') not in ('witness', 'campaign') or result['verification'] != request.get('verification'):
        raise ValueError('registered witness verification is required')
    frames = count(result.get('frames_emulated'), 'frames')
    executions = count(result.get('executions'), 'executions')
    first = result.get('first_milestone')
    if 'first_milestone' not in result:
        raise ValueError('missing first-milestone reporting cannot mean nonattainment')
    arrival = None
    if first is not None:
        if not isinstance(first, dict) or set(first) != {'execution', 'frames_emulated'}:
            raise ValueError('first milestone must contain its admitted execution and cost')
        arrival = count(first['frames_emulated'], 'arrival frames')
        execution = count(first['execution'], 'arrival execution')
        if arrival > frames or execution > executions:
            raise ValueError('milestone arrival exceeds observed work')
    attained = arrival is not None and arrival <= budget
    if result.get('milestone_within_budget') is not attained:
        raise ValueError('milestone budget flag disagrees with the recorded arrival')
    if result.get('stop_reason') == 'milestone' and not attained:
        raise ValueError('milestone stop lacks a budgeted event')
    if attained and result.get('stop_reason') not in ('milestone', 'victory'):
        raise ValueError('budgeted event disagrees with the evaluator stop reason')
    complete = frames >= budget
    if attained:
        hit, cost = True, [arrival, arrival]
    elif complete:
        hit, cost = False, [budget, budget]
    else:
        hit, cost = None, None
    return {'endpoint': condition['name'], 'milestone_stop': dict(condition),
            'cost_convention': 'first_admitted_job_frames_v1', 'budget_frames': budget,
            'arrival_exact': arrival, 'observed_through_frames': frames,
            'observed_full_budget': complete, 'hit_by_budget': hit,
            'restricted_cost_interval': cost}
