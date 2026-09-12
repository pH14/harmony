#!/usr/bin/env python3
"""Exact two-parent counterexamples for early hard exhaustion, not a native model."""
from collections import defaultdict
from fractions import Fraction as F
import json


def evaluate(threshold, horizon, useful_a, useful_b):
    # The main walk chooses the preferred class A until it exhausts. B stays
    # eligible by always yielding an irrelevant retained replacement on failure.
    # The quarter-uniform fallback assigns each parent probability 1/8.
    alive = {0: F(1)}
    success, restricted_cost = F(0), F(0)
    for _ in range(horizon):
        restricted_cost += sum(alive.values())
        next_alive = defaultdict(F)
        for failures, mass in alive.items():
            choose_a = F(7,8) if failures < min(threshold,64) else F(1,8)
            success += mass*(choose_a*useful_a+(1-choose_a)*useful_b)
            next_alive[min(failures+1,64)] += mass*choose_a*(1-useful_a)
            next_alive[failures] += mass*(1-choose_a)*(1-useful_b)
        alive = next_alive
        assert success+sum(alive.values()) == 1
    return {'hit_probability': success, 'restricted_unit_work': restricted_cost}


def serialize(row):
    return {k: {'exact': str(v), 'decimal': float(v)} for k,v in row.items()}


def examples():
    horizon=16
    results={}
    for name, a, b in [('rare_useful_preferred', F(1,16), F(0)),
                        ('irrelevant_preferred_trap', F(0), F(1,16))]:
        control=evaluate(3,horizon,a,b); candidate=evaluate(64,horizon,a,b)
        # No path can hit the cap64 in16 unit-cost expansions: independent
        # closed-form first-hit probabilities check the exact state recursion.
        q=F(7,8)*a+F(1,8)*b
        assert candidate['hit_probability']==1-(1-q)**horizon
        assert candidate['restricted_unit_work']==sum((1-q)**i for i in range(horizon))
        results[name]={'control':serialize(control),'candidate':serialize(candidate)}
    assert evaluate(64,16,F(1,16),F(0))['hit_probability']>evaluate(3,16,F(1,16),F(0))['hit_probability']
    assert evaluate(64,16,F(0),F(1,16))['hit_probability']<evaluate(3,16,F(0),F(1,16))['hit_probability']
    assert evaluate(3,3,F(1,16),F(1,8))==evaluate(64,3,F(1,16),F(1,8))
    return {'format':'entry-exhaustion-finite-model-v1','horizon_unit_work':horizon,'examples':results,
            'assumptions':['Exactly two persistent parents; main walk selects the preferred eligible class.',
                           'A non-goal attempt has no retained descendant; B always renews an irrelevant same-class representative.',
                           'Independent idealized draws and stationary stated useful-outcome probabilities; each attempt costs one unit.',
                           'No novelty, group-energy, growth, replacement geometry or in-flight scheduling is modeled.'],
            'conclusion':'Neither cutoff dominates. The model proves a possible allocation tradeoff, not its sign in an emulator. The candidate64 is the existing hard cap, not a tuned optimum.'}


if __name__=='__main__':
    print(json.dumps(examples(),indent=2))
