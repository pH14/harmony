#!/usr/bin/env python3
"""Exact same-draw finite-cap model for the implemented retry variant."""
from fractions import Fraction as F
from itertools import product
import json


def episode(draws, stages, retry):
    live, previous_death, work = 0, False, 0
    for viable in draws:
        work += 1
        if viable:
            live += 1
            previous_death = False
            if live == stages:
                return True, work
        elif retry and not previous_death:
            previous_death = True
        else:
            break
    return False, work


def expectation(stages, q, retry):
    success, cost, mass = F(0), F(0), F(0)
    for length in range(1, 7):
        for draws in product((False, True), repeat=length):
            probability = F(1, 6) * q**sum(draws) * (1-q)**(length-sum(draws))
            reached, work = episode(draws, stages, retry)
            success += probability * reached
            cost += probability * work
            mass += probability
    assert mass == 1
    return success, cost, cost/success if success else None


def model():
    rows = []
    for stages in (1,2,5,7):
        for q in (F(1,4),F(1,2),F(3,4)):
            old, new = (expectation(stages,q,retry) for retry in (False,True))
            if stages == 1: assert old[2] == new[2] == 1/q
            if stages == 7: assert old[0] == new[0] == 0
            rows.append({'stages':stages,'q':str(q),
                'control':dict(zip(('episode_success','episode_work','expected_work_to_hit'), map(lambda x: None if x is None else str(x),old))),
                'retry':dict(zip(('episode_success','episode_work','expected_work_to_hit'),map(lambda x: None if x is None else str(x),new))),
                'work_ratio':None if old[2] is None else str(new[2]/old[2])})
    assert episode([True,False,True,True],3,False)==(False,2)
    assert episode([True,False,True,True],3,True)==(True,4)
    assert episode([False,False,True],1,True)==(False,2)
    return {'format':'predrawn-local-retry-exact-model-v1','rows':rows,
        'scope':'Both policies receive the identical distribution of one-to-six pre-drawn Bernoulli attempts. Failed work counts; no extra command is drawn.',
        'limits':['Aliased chain with constant independent viability; every live action advances the goal. Not fitted to NES.',
            'Unit frame cost and zero restore emulation, not equal host overhead. No timing/memory dominance.',
            'The fixed cap removes all chance for a seven-step aliased target; it materially limits the uncapped model benefit.',
            'Complete intermediate retention and alive-trap counterexamples from local-retry-model.json still apply.']}


if __name__=='__main__': print(json.dumps(model(),indent=2))
