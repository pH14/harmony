# What returning to partial progress would have to buy

Consider exact states `s0, s1, ..., sm`, where each next state preserves useful
progress and remains extendable. If a trial at stage j costs `cj` and independently
reaches the next stage with probability `qj > 0`, exact return after failure gives
expected stage work `cj/qj`; a sequential construction then costs the sum of
those terms, plus setup/restore/verification. A full-tape attempt instead must
traverse every stage in one attempt. Under the additional assumptions of fixed
conditional probabilities and identical stages on successful paths, its success
probability is the product of the `qj`. Expected trial cost must include early
failures, so comparing the two success expressions alone is not a work ratio.

These are conditional renewal identities, not a model already validated for
Metroid. Enemy HP alone is not a sufficient state: health, position, velocity,
projectiles, timing and enemy status jointly determine the continuation law.
Damage can consume the resources or positioning needed for the next step. A
retained lower-HP enemy state can therefore have a worse continuation value.
Exact restoration preserves these consequences rather than repairing them.
The source archive omits enemy HP, but S01 does not demonstrate an actual
replacement of its damage state by a worse same-cell state.

S01 supplies one exact state after one surviving HP decrease. It supplies no
validated probability for the next decrease, no memoryless damage process, no
retention/selection comparison and no full-fight expectation. Its 1/32 observed
frequency must not be raised to a power, reused at every HP level, or used as a
sample-size/power calculation. A useful inexpensive falsifier of the immediate
return-and-extend premise is to try the already frozen suffixes from the first
saved damage state and distinguish further surviving progress from inherited
projectile effects and rapid death.

## S02 decision, before outcomes

Use only the first S01 ordinary surviving-damage witness, whose two held replays
already agree. Its route prefix is 118,143 frames plus 929 setup frames. Preserve
all resources and state, including boss HP 139 and health 79 tenths. Reuse the
unchanged 32-seed / 128-command S01 draw list and the same passive comparator,
arm order, 8,192-frame continuation ceiling and damage/witness rules. No input
law tuning, new seed list, resource injection or fresh-search evidence is allowed.
The selected root and reused suffixes are dependent; results are descriptive
conditional evidence, not independent confirmation or a causal HP-only contrast.

A further surviving HP decrease or defeat requires two held replays of the
complete new input. Passive progress must be reported before attributing a
change to new button presses. No positive earns an automatic third selected
root; a negative closes this two-stage diagnostic without replacement seeds.
Either result informs whether retention of partial damage is even a productive
next design question; neither alone promotes a generic archive or controller.

Use the unchanged qualified native binary. Freeze exact prefix, root, assets,
build, launcher and output identities before dispatch. Reserve a separate 2M
known-auxiliary ceiling, 570-second process wall limit / 600-second service cap,
4 GiB memory and 64 MiB output. Maximum planned work is 1,661,472 frames including
at most four twice-replayed witnesses. No new admitted performance search runs.
