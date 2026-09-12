#!/usr/bin/env python3
"""Exact finite toy costs for one bounded local retry; not fitted to NES data."""
from fractions import Fraction as F
import json


def cost(stages, viable_probability, attempts):
    # Aliased intermediate states are lost on a failed rollout. Each unit-cost
    # attempt is independent. A local restore costs zero *emulated* work here.
    q = viable_probability
    stage_pass = 1 - (1-q)**attempts
    stage_cost = stage_pass / q
    episode_pass = stage_pass**stages
    episode_cost = stage_cost * sum(stage_pass**j for j in range(stages))
    return episode_cost / episode_pass


def model():
    rows = []
    for k in (1, 2, 5):
        for q in (F(1,4), F(1,2), F(3,4)):
            baseline, retry = (cost(k, q, r) for r in (1,2))
            assert k != 1 or baseline == retry == 1/q
            rows.append({'stages': k, 'viable_probability': str(q),
                         'baseline_expected_attempts': str(baseline),
                         'one_retry_expected_attempts': str(retry),
                         'ratio': str(retry / baseline),
                         'perfectly_retained_optimal_return_cost': str(k / q)})
    # Equal chance of immediate success or an alive trap. In the trap, every
    # next action dies. Retrying the dead action wastes one more unit of work.
    trap_baseline = (F(1,2)*1 + F(1,2)*2) / F(1,2)
    trap_retry = (F(1,2)*1 + F(1,2)*3) / F(1,2)
    assert (trap_baseline, trap_retry) == (3,4)
    return {'format': 'local-retry-finite-model-v1', 'rows': rows,
            'alive_trap': {'baseline_expected_attempts': str(trap_baseline),
                           'one_retry_expected_attempts': str(trap_retry)},
            'assumptions': ['Independent constant-q attempts along a finite chain; every live step is useful in the favorable model.',
                            'Intermediate states alias in the global archive and a failed rollout restarts the episode.',
                            'All failed attempts count; local snapshot restore has zero emulated-frame cost, not zero wall cost.',
                            'This is a regenerative hitting-cost calculation, not a fixed-budget pass probability or a model fitted to Ridley.'],
            'limits': ['Full intermediate retention removes the favorable model\'s exponential reset penalty.',
                       'Alive traps are a strict adverse case; viability alone is not useful progress.',
                       'Finite command/time limits and changing hazard can remove the advantage. No native allocation follows the model alone.']}


if __name__ == '__main__':
    print(json.dumps(model(), indent=2))
