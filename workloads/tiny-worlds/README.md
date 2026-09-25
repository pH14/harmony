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

## Scenario chains

A `chain` config contains one to sixteen `stages`, each with a leaf `world` and
an explicit `refill_available` flag. Nested chains are rejected. Completing a
stage enters the next stage during the same action; only the last stage's goal
is the campaign objective. The entire chain uses one campaign, archive, work
budget, and start-to-end witness. Snapshots contain the active stage, local
state, and carried charge. Stage-local health, history, and environmental clocks
reset on entry; this version does not model physical movement back to prior
stages. The archive can still restore earlier states.

With `carry_charge=false`, each stage starts from its declared initial state
and chain `initial_charge` must be zero. The normal key namespaces place by
stage; the broken control omits only that namespace. Local mechanics and local
representation stay unchanged. With `carry_charge=true`, chain `initial_charge`
initializes ammunition, resource stages consume/replenish it, and other stages
preserve it. Resource stages must share their capacity and declare local
`initial_charge=0`, since chain entry explicitly overrides that field. The
broken control omits carried stock from retention preferences; ordinary local
charge/time/health preferences remain intact. It tests carrying useful stock
through intervening stages, not eliminating replenishment at its source.

`refill_available=false` disables action 1 only in a resource stage, allowing a
late barrier without a second replenishment station. Other families require
that flag to be true. This boundary contract is part of the world mechanics,
identical in both comparison arms. Whole-chain reachability is enumerated with
a 100,000-state limit; individual stage reachability is not sufficient.

All chains use the uniform four-action alphabet and a 512-action path limit,
including the one-stage chain baseline. Standalone worlds retain their existing
samplers and 128-action limit. Chain work-admission overshoot is consequently
bounded by 511 transitions. Stage number distinguishes places but is not a
progress score or retention preference. Carried stock adds the leading component
to the charge-first preference and the last component to the health-first one;
standalone worlds have zero stock, preserving their existing preference order.
Across visible stage namespaces, preferences compare only persistent stock;
local ammunition, clocks, and health have different meanings across stages.
This also applies to continuation edge comparisons, not just slot retention.
The omitted-stage control intentionally loses this boundary distinction too.

Diagnostics record first observed stage-entry work, entry charge distributions,
suffix actions per stage, and nonempty executed jobs classified by their actual
parent stage/charge. Separate pre-objective counters exclude post-completion
work; their sum is checked against work to the first objective, or total work
when unsolved. Parent work includes replay and is checked against the
independent campaign-stream work sum. It is not a count of all selector attempts
or skipped jobs. First-entry work uses the single worker's cumulative execution
counter; restoring a snapshot does not rewind it. These measurements never feed
selection. Intermediate stage completion is not a terminal objective or a new
search invocation. Known paths are used only in mechanics/replay verification.

Carry controls use the engine's ordinary path-length tie-break when hidden stock
leaves otherwise equal keys. Refills cost actions, so this tends to retain lower
stock; the comparison is stock-aware retention versus that default tie-break,
not a neutral resource-blind policy. Omitting stage identity deliberately causes
later states to collide with earlier, shorter paths. Neither control alone
establishes improved continuation-graph propagation.

Pre-objective continuation job/work counters exclude post-success dispatches and
include the objective job. These measure continuation activity, not causal
attribution of useful stock reaching the final barrier. The chain does not model
position-sensitive replay after a kill, persistent capability identities, or a
selection ladder with stage-dependent progress tiers. Its local states have
exact deterministic transitions and stock does not alter movement geometry.

## Upgrade and position-sensitive route reuse

The `route` world scouts a deterministic route to a blocked endpoint, returns to
its origin, acquires a persistent upgrade there, and must traverse the route
again. `pattern` supplies two-bit actions per position, `length` is 2–16,
`attack` returns from the blocked endpoint and acquires the upgrade at the origin.
Wrong route actions return to the origin without losing the upgrade. In the
`shifted` variant, acquisition changes the starting lane. The alignment action,
`(attack + 1) % 4`, must restore the original lane before route movement works.
Alignment differs from the first route action. With `upgrade_required=false`,
the first traversal completes the objective and no upgrade is available.

Position and lane identify archive places; scout/upgrade phase is a retention
preference. Both comparison arms use exactly this key, the same sampler, and the
same world transitions. The `broken` flag has no effect on this family. Route
worlds cannot currently be embedded in chains. This isolated diagnostic supplies
no oracle tape or pre-populated route bank to the searcher. It deliberately
requires scouting before acquisition, so it tests reuse after route discovery
rather than whether an unconstrained game naturally discovers things in that order.

Route evidence joins executed action observations to campaign job sequence IDs
and admission decisions. It records the selector responsible for each first
upgraded position/lane arrival, the first objective, and pre-objective continuation transfers
that advance an upgraded state. Donor and leaf states come from historical
retained admissions, including entries no longer resident at the end. A transfer
is labeled non-upgraded when both recorded donor and leaf have phase below 2.
This describes their state, not whether their admission preceded the campaign's
first acquisition. First acquisition and first alignment jobs are recorded
separately, including their work and sequence. Alignment is one action with
probability 1/4 under the sampler. Reports distinguish alignment from acquisition,
even if they share a job.
Job work includes execution/replay overhead; these timestamps are job-end work,
not per-action first-hit times. Raw bounded traces remain in local reports and
are included in replay verification. No evidence changes selection.

The `benchmarks/tiny_worlds/panel.py --route-reuse` command builds one private
source copy with a diagnostic environment check at continuation-bank creation.
Both arms execute the exact same binary. The runner sets or clears
`TINY_WORLD_DISABLE_CONTINUATIONS` before each process starts, records the control
value and executable hash, and independently verifies that arm's rerun and replay
under the same fixed setting. A frozen unmodified source copy and source hashes
verify that instrumentation changes exactly one engine site. The runner requires
zero continuation dispatches in the disabled arm. Production engine code has no
new option or change.

The family's alphabet-only mixture does not dispatch ordinary splicing in either
arm. The comparison is continuations versus no tail reuse under that mixture;
the disabled arm can still restore archived states and draw random one- or
two-action suffixes. Both arms keep identical archive preferences, mechanics,
and budgets. Equal seeds cease to imply matching action samples after dispatch
schedules diverge. The no-upgrade control queues no continuations, so identical
outcomes there check isolation, not dispatch overhead.

Every observed transfer in the registered panel is a one-action transition.
Reusing a route means propagating the upgrade through those transitions; these
cases do not test long open-loop action tapes. The shifted variant introduces
only a one-action alignment requirement, not general post-kill route repair.

A successful saved-route replay establishes local compatibility, while a paired
campaign comparison measures whether automatic reuse helps under the registered
budget. Keep those conclusions separate. The two-lane alignment mechanic models
one position-sensitive failure; it does not reproduce game physics, combat,
capability-dependent geometry, or full-game route robustness.
