# SPDX-License-Identifier: AGPL-3.0-or-later
"""Controller-only benign qualification cases; never stage this module for agents."""
from __future__ import annotations

from dataclasses import asdict, dataclass
import hashlib
import json


@dataclass(frozen=True)
class Behavior:
    arguments: tuple[str, ...]
    expected: tuple[int, ...]


@dataclass(frozen=True)
class CheckerControl:
    arguments: tuple[str, ...]
    holds: bool


@dataclass(frozen=True)
class Case:
    name: str
    split: str
    source: bytes
    behavior: tuple[Behavior, ...]
    controls: tuple[CheckerControl, ...]


# Paired arms receive selected source, the same neutral task and factual product
# docs. Only treatment receives the previously frozen generic skills.
# Reference outcomes, controls, split assignments and qualification solutions
# stay in the controller. These are finite behavioral contracts, not a proof of
# arbitrary C program equivalence.
TASK = ('Add a correctness checker for this program and prepare it for Harmony. '
        'Preserve the existing apply interface. Check arguments contain apply inputs '
        'followed by observed output integers. Choose the property from '
        'the source; the property function receives the check arguments and returns '
        '1 when its property holds, 0 when it does not, or -1 for unsupported input.')

_STUB = 'static int property(int argc, char **argv) { (void)argc; (void)argv; return -1; }'
_PREFIX = '''#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
'''
_DRIVER = '''
int main(int argc, char **argv) {
    if (argc < 2) return 64;
    if (!strcmp(argv[1], "apply")) return apply(argc - 2, argv + 2);
    if (!strcmp(argv[1], "check")) {
        int holds = property(argc - 2, argv + 2);
        if (holds != 0 && holds != 1) return 64;
        puts("@reachable 7");
        printf("@always 8 %d\\n", holds);
        return 0;
    }
    if (!strcmp(argv[1], "ready")) return 0;
    if (!strcmp(argv[1], "idle")) { for (;;) pause(); }
    return 64;
}
'''
_COUNTER = '''static int apply(int argc, char **argv) {
    if (argc != 2) return 64;
    int initial = atoi(argv[0]), delta = atoi(argv[1]);
    int next = initial + delta;
    if (next < 0) next = 0;
    if (next > 4) next = 4;
    printf("%d\\n", next);
    return 0;
}
'''
_TRANSFER = '''static int apply(int argc, char **argv) {
    if (argc != 3) return 64;
    int left = atoi(argv[0]), right = atoi(argv[1]), amount = atoi(argv[2]);
    if (amount >= 0 && amount <= left) { left -= amount; right += amount; }
    printf("%d %d\\n", left, right);
    return 0;
}
'''


def cases() -> tuple[Case, ...]:
    counter = tuple(
        Behavior(('apply', str(initial), str(delta)), (max(0, min(4, initial + delta)),))
        for initial in range(5) for delta in range(-2, 3)
    )
    transfer = tuple(
        Behavior(('apply', str(left), str(right), str(amount)),
                 (left - amount, right + amount) if 0 <= amount <= left else (left, right))
        for left in range(3) for right in range(3) for amount in range(-1, 4)
    )
    return (
        Case('bounded-counter', 'development', (_PREFIX + _COUNTER + _STUB + _DRIVER).encode(), counter,
             (CheckerControl(('check', '0', '-2', '0'), True),
              CheckerControl(('check', '3', '2', '4'), True),
              CheckerControl(('check', '2', '-1', '1'), True),
              CheckerControl(('check', '0', '-2', '-2'), False),
              CheckerControl(('check', '3', '2', '5'), False),
              CheckerControl(('check', '2', '-1', '2'), False))),
        Case('two-account-transfer', 'held-out', (_PREFIX + _TRANSFER + _STUB + _DRIVER).encode(), transfer,
             (CheckerControl(('check', '2', '1', '1', '1', '2'), True),
              CheckerControl(('check', '1', '2', '2', '1', '2'), True),
              CheckerControl(('check', '0', '2', '0', '0', '2'), True),
              CheckerControl(('check', '2', '1', '1', '1', '3'), False),
              CheckerControl(('check', '1', '2', '2', '-1', '4'), False),
              CheckerControl(('check', '0', '2', '0', '1', '2'), False))),
    )


def qualification_source(case: Case, variant: str) -> bytes:
    """Hand-authored controls for no-model qualification, not agent material."""
    if variant == 'valid':
        if case.name == 'bounded-counter':
            body = '''if (argc != 3) return -1;
    int value = atoi(argv[0]) + atoi(argv[1]);
    int observed = atoi(argv[2]);
    return observed == (value < 0 ? 0 : value > 4 ? 4 : value);'''
        elif case.name == 'two-account-transfer':
            body = '''if (argc != 5) return -1;
    int left = atoi(argv[0]), right = atoi(argv[1]), amount = atoi(argv[2]);
    int moved = amount >= 0 && amount <= left ? amount : 0;
    return atoi(argv[3]) == left - moved && atoi(argv[4]) == right + moved;'''
        else:
            raise ValueError('unknown qualification case')
    elif variant in ('always-pass', 'always-fail'):
        body = '(void)argc; (void)argv; return %d;' % (variant == 'always-pass')
    elif variant == 'silent':
        source = qualification_source(case, 'valid')
        lines = source.splitlines(keepends=True)
        return b''.join(b'        puts("checker completed");\n' if b'puts("@reachable' in line
                        else b'        (void)holds;\n' if b'printf("@always' in line
                        else line for line in lines)
    elif variant == 'changed-application':
        source = qualification_source(case, 'valid')
        # This preserves plausible checker output while changing real apply
        # behavior. It must fail behavioral grading before receiving credit.
        return source.replace(b'return apply(argc - 2, argv + 2);', b'{ puts("0"); return 0; }')
    else:
        raise ValueError('unknown qualification submission variant')
    replacement = ('static int property(int argc, char **argv) {\n    ' + body + '\n}').encode()
    return case.source.replace(_STUB.encode(), replacement)


def validate_cases(selected: tuple[Case, ...]) -> None:
    """Bind qualification to both reviewed domains and every private control.

    This version covers 25 counter inputs, 45 transfer inputs, and three valid
    plus three invalid checker observations per case. Changing any case bytes,
    split, input, outcome, or count requires an explicit corpus version review.
    """
    records = []
    for case in selected:
        record = asdict(case)
        record['source'] = case.source.hex()
        records.append(record)
    encoded = json.dumps(records, sort_keys=True, separators=(',', ':')).encode()
    if hashlib.sha256(encoded).hexdigest() != '9228dc6a79cc1098506871316fa9ee9604998bf76247a5c5e0cbdf0541be4e99':
        raise ValueError('qualification corpus differs from the reviewed two-case contract')
