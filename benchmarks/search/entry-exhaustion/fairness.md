# Uniform access is a weak finite-budget allocation guarantee

This note was derived after ED01 registration while its first wave was running.
It changes no hypothesis, candidate, endpoint, parameter, budget or stopping rule.
The prospective favorable/adverse examples remain in `model.py`.

Assume a fixed parent E is guaranteed to remain in the uniform selector's
eligible index for the next H reservations on every possible history, rather
than conditioning afterward on an observed survivor. Let N_t be the index size
at reservation t and
suppose N_t ≤ M. Assume ideal fresh random choices with the code's one-quarter
uniform component. Conditional on any prior history in which E remains eligible,

    P(select E next | history) ≥ 1 / (4 N_t) ≥ 1 / (4 M).

The main walk may add probability; an exhausted entry need not receive any of
that share. Induction on the conditional no-selection probabilities gives

    P(E not selected in H reservations) ≤ (1 - 1/(4M))^H.

This bound allows the archive and other selector weights to adapt. It does not
require N_t to be fixed or those histories to be independent. If the eligibility
and cardinality assumptions hold indefinitely, summing the tail bound gives
E[reservations until selection] ≤ 4M. Equality is possible in a fixed archive
when E gets only its uniform share and another class keeps the main walk alive.

| Eligible-population bound M | Upper bound on expected reservations | Reservations sufficient for ≥95% selection probability under these assumptions |
| ---: | ---: | ---: |
| 100 | 400 | 1,197 |
| 1,000 | 4,000 | 11,982 |
| 10,000 | 40,000 | 119,828 |

These are mathematical examples, not measurements of a game or current archive.
The 95% column is `ceil(log(0.05) / log(1 - 1/(4M)))`. A memory/entry cap makes M
finite; it does not make that worst-case reservation count small. Replacement,
loss of eligibility or the campaign ending invalidates an indefinite-wait claim.

Selection is also weaker than executed continuation, and much weaker than a
useful outcome. Recorded duplicate skips can consume a selection without new
emulator work. A useful-event bound would additionally need a justified positive
lower bound on its conditional probability after selection, including those
skips and the actual suffix law. None of the completed probes establishes such
a bound for a fresh boss defeat. This argument therefore cannot supply a boss
success probability, sample-size guarantee or native-work efficiency claim.

The actual finite PRNG is deterministic; the ideal conditional-randomness model
is not a proved probability bound for native seed sampling. The Rust fixture and full native replay
check the implemented selection paths separately. The source does prove that
cutoff3 removes eligibility for the main walk after three unproductive jobs,
while the uniform path bypasses that cutoff. It does not prove that cutoff64
allocates work better: the already frozen adverse model shows the opposite when
the preferred class is an irrelevant trap. ED01 tests that unresolved empirical
sign and stays closed if its registered gate fails.
