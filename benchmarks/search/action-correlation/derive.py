#!/usr/bin/env python3
"""Exact finite proposal-law checks. No ROM, emulator, trajectories or fitted data."""
from fractions import Fraction as F
import hashlib
from itertools import product
import json

ACTIONS = tuple(product(range(9), range(2), range(2)))
CARDINALITIES = (9, 2, 2)
ALPHA = F(1, 2)


def refresh_one(x, y):
    return sum((F(1, 3 * CARDINALITIES[i])
                for i in range(3) if all(x[j] == y[j] for j in range(3) if j != i)), F(0))


def kernel(kind):
    component_diagonal = (1 - ALPHA) / 36 + ALPHA * refresh_one(ACTIONS[0], ACTIONS[0])
    whole_repeat = (component_diagonal - F(1, 36)) / (1 - F(1, 36))
    def probability(x, y):
        if kind == 'iid':
            return F(1, 36)
        if kind == 'component_half':
            return (1 - ALPHA) / 36 + ALPHA * refresh_one(x, y)
        if kind == 'matched_whole_repeat':
            return (1 - whole_repeat) / 36 + whole_repeat * (x == y)
        raise ValueError(kind)
    return [[probability(x, y) for y in ACTIONS] for x in ACTIONS]


def extend_menu(matrix):
    # State 36 is the existing special tap. It resets command correlation.
    ordinary = [[F(11, 12) * p for p in row] + [F(1, 12)] for row in matrix]
    return ordinary + [[F(11, 12 * 36)] * 36 + [F(1, 12)]]


def row_event(matrix, event):
    values = [sum((p for y, p in zip(ACTIONS, row) if event(x, y)), F(0))
              for x, row in zip(ACTIONS, matrix)]
    assert len(set(values)) == 1, 'closed form needs state-independent event probability'
    return values[0]


def encode(value):
    if isinstance(value, F):
        return {'exact': str(value), 'decimal': float(value)}
    if isinstance(value, dict):
        return {key: encode(item) for key, item in value.items()}
    if isinstance(value, (tuple, list)):
        return [encode(item) for item in value]
    return value


def main():
    matrices = {name: kernel(name) for name in ('iid', 'component_half', 'matched_whole_repeat')}
    stats = {}
    for name, matrix in matrices.items():
        assert all(sum(row) == 1 and min(row) > 0 for row in matrix)
        assert all(sum(row[j] for row in matrix) == 1 for j in range(36))
        assert all(matrix[i][j] == matrix[j][i] for i in range(36) for j in range(36))
        augmented = extend_menu(matrix)
        stationary = [F(11, 12 * 36)] * 36 + [F(1, 12)]
        assert all(sum(row) == 1 for row in augmented)
        assert [sum(stationary[i] * augmented[i][j] for i in range(37)) for j in range(37)] == stationary
        same = row_event(matrix, lambda x, y: x == y)
        d = row_event(matrix, lambda x, y: x[0] == y[0])
        da = row_event(matrix, lambda x, y: x[:2] == y[:2])
        all_change = row_event(matrix, lambda x, y: all(a != b for a, b in zip(x, y)))
        stats[name] = {
            'whole_chord_same': same,
            'direction_same': d,
            'direction_and_a_same': da,
            'all_components_change': all_change,
            'matrix_sha256': hashlib.sha256(json.dumps([[str(p) for p in row] for row in matrix]).encode()).hexdigest(),
            'four_regular_chord_patterns': {
                'direction_preserved_and_a_changes_at_least_once': d**3 - da**3,
                'all_components_change_at_every_boundary': all_change**3,
            },
            'one_to_six_ordinary_chord_mean_patterns': {
                # Includes length one, where neither transition pattern is counted.
                'direction_preserved_and_a_changes_at_least_once': sum((d**k-da**k for k in range(1,6)),F(0))/6,
                'all_components_change_at_every_boundary': sum((all_change**k for k in range(1,6)),F(0))/6,
            },
            'full_chord_run_survival_at_regular_boundaries': [same**k for k in range(1,6)],
        }
    candidate, matched = stats['component_half'], stats['matched_whole_repeat']
    assert candidate['whole_chord_same'] == matched['whole_chord_same'] == F(43,216)
    assert candidate['full_chord_run_survival_at_regular_boundaries'] == matched['full_chord_run_survival_at_regular_boundaries']
    assert candidate['direction_same'] == F(11,27)
    assert candidate['direction_and_a_same'] == F(8,27)
    positive = 'direction_preserved_and_a_changes_at_least_once'
    negative = 'all_components_change_at_every_boundary'
    assert candidate['four_regular_chord_patterns'][positive] > matched['four_regular_chord_patterns'][positive] > stats['iid']['four_regular_chord_patterns'][positive]
    assert candidate['four_regular_chord_patterns'][negative] < matched['four_regular_chord_patterns'][negative] < stats['iid']['four_regular_chord_patterns'][negative]
    # The injected "just repeat the whole chord" replacement must fail the
    # partial-component claim despite passing the full-chord run-length match.
    assert matched['direction_same'] != candidate['direction_same']
    expected_hold = F(11,12) * (F(2+12,2)+F(48+120,2))/2 + F(1,12)*F(2+7,2)
    assert expected_hold == F(505,12)
    report = {'format':'action-correlation-exact-model-v1',
              'candidate_alpha':ALPHA,'derived_whole_repeat_probability':F(37,210),
              'actions':36,'special_tap_probability':F(1,12),
              'expected_proposed_frames_per_chord':expected_hold,
              'expected_proposed_frames_per_one_to_six_suffix':expected_hold*F(7,2),
              'law_checks':'exact row/column sums, reversibility, full-support stationarity, special-tap reset stationarity and matched full-chord run lengths passed',
              'kernels':stats,'emulator_frames':0,
              'limits':['Ideal independent uniform draws; finite PRNG output sequences need separate implementation/replay checks.',
                        'Pattern probabilities condition on ordinary chords; real special taps reset correlation.',
                        'Matched proposed frame laws do not force equal actual work after terminal boundaries.',
                        'Constructed rewarding and adverse patterns refute utility dominance; neither is evidence of Metroid or MM2 efficacy.']}
    print(json.dumps(encode(report),indent=2))


if __name__ == '__main__':
    main()
