<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Tiny deterministic worlds

This workload runs original, bounded worlds through Dissonance's real campaign
engine. It supplies mechanics, archive representation, action sampling, snapshots,
objective evaluation, and diagnostics. It contains no alternative search engine
and needs no emulator or licensed assets. The engine baseline is
`67f65bbce5ee84b57cd91aaceb49350325eb49b9` (fetched main, including the two-level
archive consolidation). The original local main branch was behind that revision;
historical game outcomes do not describe this baseline.

The resource world compares two representations of identical mechanics. Both
retain one holder per charge-first and health-first preference, use the same
place/progress/identity definitions, and keep engine continuation enabled.
The broken control reports zero charge in its key. This deliberately hides
replenishment improvements; it does not remove replenishment from the world.
No shortest-path actions or oracle states are supplied to search.

Resource propagation uses these same mechanics. The registered `resource-propagation`
development case and `resource-propagation-heldout` validation case refill stock,
then carry it through multiple route transitions that consume charge before the final
barrier. This makes replenishment propagation across multiple transitions an explicit
panel case, rather than a separate mechanic.

The binary reads one strict JSON request from standard input, capped at 16 KB.
Required fields are `config`, `seed`, `work_budget` (1–20,000 transitions),
`broken`, and `verify`. It validates the world and independently enumerates
reachable states before searching. Output is one compact JSON report. See the
[registered panel](../../benchmarks/tiny_worlds/panel.json) for requests and the
[runner instructions](../../benchmarks/tiny_worlds/README.md) for bounded runs.

Search uses one worker and one reservation, an action cap of 128, an archive cap
of 4,096, and a 32 MiB logical memory budget. The logical archive charge is not an
RSS cap. The event stream has a checked 32 MB allocation bound; full streams are
not exported. Verification replays the stream, repeats fixed-work execution,
checks the objective witness directly against world mechanics, and sums admitted
job work independently. Work includes restored-parent replay and suffix actions;
world environmental state and the cumulative execution counter remain separate.

Changes must pass exact correctness checks. Seed-panel solve fractions, Wilson
intervals, and censored work-to-objective rows are descriptive comparisons, not
universal search-quality thresholds. Mechanics and local game correspondence do
not establish a search-policy ranking; comparative claims belong to the registered
panels. The delayed-progress family remains exploratory without a verified game
correspondence.

Diagnostic arrivals count transitions into the barrier location, not repeated
observations there. Refill counts require an actual stock increase. These
counters cover admitted suffix observations and exclude restored-parent replay;
the logical-work total includes that replay. Exported archive entries include
history and must not be interpreted as active holders; `live_entries` is reported
separately. Build-time engine and workload source hashes accompany the executable
hash, and the panel checks the engine hash and request echoes before scoring.
Wall-clock telemetry is an explicit Clippy exception and never feeds search.

## Family contracts and evidence

| Family | Failure and software analogue | Assumptions and omitted details | Expected diagnostic | Status |
| --- | --- | --- | --- | --- |
| Resource barrier | Useful consumables disappear from retained states; quota replenishment before a multi-step operation. | Deterministic refill station, known charge/health state, consuming corridor; no combat, random drops or navigation. | Charge at barrier arrivals, successful refills/heals, stock preference replacement, continuation work. | Metroid local mechanism calibrated; see the registered results. |
| History maze | Equal visible locations have different futures after earlier choices; a protocol requiring a prior handshake sequence. | Bounded raw choice history is observable in the sufficient representation; no spatial movement or game-specific RAM abstraction. | Correct/wrong history arrivals, loop resets, distinct versus colliding keys. | SMB 7-4 local mechanism calibrated; see the registered results. |
| Changing actions | An action useful in one regime stops producing progress; a service changing supported operations. | Land/water/land stages, a fixed action alphabet, optional observable regime; no inertia, hazards or learned controller. | Attempts and advances by regime/action, entry to the changed regime, recovery in the final land segment. | SMB local mechanism calibrated; see the registered results. |
| Deadline corridor | A route reaches shared positions with more or less environmental time before a late obstacle. | Fast route segments take two actions and `fast_ticks` per action; slow segments take one action and `slow_ticks`; no stochastic travel or combat. | Remaining time on progressing arrivals, obstacle attempts, and expiration before the objective. | SMB 8-1 local clock-preference mechanism calibrated; native control reverses ranking. |
| Delayed progress | A useful sequence or wait must continue without intermediate reward while distractions compete for work. | Sequence mode advances on repeated action 0; wait mode advances on repeated action 3; distractors cycle or return to the useful lane. | Useful-lane and distraction action counts, progress observations, and objective discovery. | Exploratory; MM2 correspondence is unverified. |

The deadline corridor has a fast and a slow route to each shared position. Fast
travel uses action 1 twice per segment and spends `fast_ticks` on each action;
slow travel uses action 0 once per segment and spends `slow_ticks`. The two routes
therefore differ in both campaign actions and environmental time. At the final
position, action 2 spends `obstacle_ticks`; exact payment succeeds, while a
shortfall expires the clock. Other supported actions spend one environmental
tick without advancing and clear a partial fast-route phase. Campaign work still
counts one transition per action, independently of the world's remaining-time
field. The normal key includes remaining time in `charge` and the in-progress
route phase in `context`. Its broken control hides time but keeps the phase
context, so fast and slow arrivals at a shared phase collide only in that arm.
Because `health` is zero in this family, the two existing preference orders
coincide on remaining time.

The `deadline_actions` interaction combines the changing-action stages with an
environmental clock. Every action spends its configured duration, including an
ineffective action that does not advance the stage. The objective must be reached
before expiration; exact-time completion is allowed. Its key preserves remaining
time and the action world’s configured regime context. The broken control freezes
the sampled action to the land action only; it does not hide the clock or alter
the action-world mechanics. The two preference orders also coincide when the
interaction's `health` key component is zero.

Delayed progress has sequence and wait modes. Progress is unscored until the
configured horizon is reached. The useful lane records how much of that sequence
has been completed; action 1 enters a cycle of distraction lanes, action 2 returns
to the useful lane, and actions 0 or 1 move among distractions. The normal key
retains progress as its context identity; the broken key omits that context while
leaving transitions unchanged. These worlds expose progress and action outcomes,
not an oracle path.

All worlds enumerate bounded reachability independently of campaign sampling.
The maze stores raw action history; the pattern is used only by the world and
post-run evaluator. Reversed mappings test physical action relabeling. The action
world's unobserved regime deliberately makes distinct states share a key. Its
normal arm uses the existing half-biased input draw; the broken arm freezes the
land action. This establishes the need for continued action exploration, not an
optimal adaptation algorithm. Resource, maze, deadline, and delayed controls alter
only representation; `deadline_actions` instead uses the documented frozen-action
control.

## Configuration bounds

Every field is required, nested objects reject unknown fields, and integer fields
must use their declared integer type. These bounds keep each independent oracle
below 100,000 states.

| Family | Bounds |
| --- | --- |
| Resource | `initial_charge` 0–31; `initial_health` 1–31; `barrier_charge` 1–15; `route_cost`, `health_cost`, and `refill_health_cost` 0–15; `refill_amount` and `max_charge` 1–31; `corridor_len` 1–12; `initial_charge` cannot exceed `max_charge`. |
| History maze | `length` 1–8; `pattern` is less than `1 << length`; `reverse_actions` is boolean. |
| Changing actions | `segment_len` 1–8; `water_action` 1–3; `return_to_land` and `observable` are boolean. |
| Deadline corridor | `length` 1–8; `initial_time` 1–64; `fast_ticks` 1–4; `slow_ticks` is greater than twice `fast_ticks` and at most 16; `obstacle_ticks` 1–16. |
| Delayed progress | `horizon` 1–16; `distractions` 1–32; `mode` is `sequence` or `wait`. |
| Deadline/actions interaction | Uses the changing-actions bounds above; `initial_time` 1–64; each of four `action_ticks` values 1–16. |

Deadline arrival histograms count transitions that advance the route or action
stage, not idle/wasted clock ticks. The deadline/action interaction also counts
action attempts and expirations. The delayed progress histogram bins every
after-state, so bin zero includes distraction-lane observations as well as
zero-progress useful-lane observations; separate useful-lane and distraction
action counters disambiguate those observations. Diagnostic observations never
supply paths or scoring hints to selection. Action diagnostic counts exclude
already-completed parents; continuation jobs/work are reconstructed from the
event stream.

The shared request now carries a tagged `config` with `family` and `parameters`,
and `broken` selects the family's documented control. Every nested parameter is
required and unknown fields are rejected. No compatibility path is retained for
the initial resource-only request.
