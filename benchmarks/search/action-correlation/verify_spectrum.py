#!/usr/bin/env python3
"""Check the frozen kernels on a complete tensor-contrast basis; no game data."""
from collections import Counter
from fractions import Fraction as F
from itertools import product
import json

from derive import ACTIONS, CARDINALITIES, encode, extend_menu, kernel


def contrast(index, value, size):
    if index == 0:
        return 1
    return int(value == index - 1) - int(value == size - 1)


def main():
    basis = []
    # In each coordinate, a constant and n-1 contrasts form a basis. Their
    # tensor products therefore span all functions on the 36-command alphabet.
    for indices in product(*(range(n) for n in CARDINALITIES)):
        values = []
        for action in ACTIONS:
            value = 1
            for i, x, n in zip(indices, action, CARDINALITIES):
                value *= contrast(i, x, n)
            values.append(value)
        degree = sum(i != 0 for i in indices)
        assert any(values)
        if degree:
            assert sum(values) == 0
        basis.append((degree, values))
    counts = Counter(degree for degree, _ in basis)
    assert counts == {0: 1, 1: 10, 2: 17, 3: 8}
    records = {}
    for name in ('iid', 'component_half', 'matched_whole_repeat'):
        matrix, augmented = kernel(name), extend_menu(kernel(name))
        values_by_degree = {}
        for degree, vector in basis:
            eigenvalue = (F(1) if degree == 0 else F(0) if name == 'iid'
                          else F(3 - degree, 6) if name == 'component_half' else F(37, 210))
            assert [sum(p * v for p, v in zip(row, vector)) for row in matrix] == [eigenvalue * v for v in vector]
            if degree:
                extended = vector + [0]
                assert [sum(p * v for p, v in zip(row, extended)) for row in augmented] == [F(11, 12) * eigenvalue * v for v in extended]
            values_by_degree[degree] = eigenvalue
        # The extra tap/ordinary contrast has eigenvalue zero: the probability
        # of a tap is independent of the current command.
        tap_contrast = [1] * 36 + [-11]
        assert all(sum(p * v for p, v in zip(row, tap_contrast)) == 0 for row in augmented)
        records[name] = {'ordinary_eigenvalue_by_interaction_degree': values_by_degree,
                         'basis_vectors_checked': 36, 'augmented_special_tap_dimension': 37}
    print(json.dumps(encode({'format': 'action-correlation-spectrum-v1',
        'ordinary_basis_multiplicities': dict(counts), 'kernels': records,
        'interpretation': 'The candidate preserves individual-component contrasts more strongly than the matched whole-command control, but preserves two-component contrasts slightly less and erases three-component contrasts after one ordinary transition.',
        'limits': ['Ideal finite kernels only; no game outcome, PRNG independence or useful-discovery claim.',
                   'Special taps multiply the nonconstant eigenvalues by 11/12; suffix boundaries start independently.',
                   'Analytical explanation added after panel registration; no policy, parameter or decision gate changes.'],
        'emulator_frames': 0}), indent=2))


if __name__ == '__main__':
    main()
