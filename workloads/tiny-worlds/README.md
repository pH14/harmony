<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Tiny deterministic worlds

This workload runs original, bounded worlds through Dissonance's real campaign
engine. It supplies mechanics, archive representation, uniform actions, snapshots,
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
universal search-quality thresholds. The initial resource family is motivated,
not game-calibrated. Calibration must establish the same mechanism using actual
local game traces before that status changes.

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
| Resource barrier | Useful consumables disappear from retained states; quota replenishment before a multi-step operation. | Deterministic refill station, known charge/health state, consuming corridor; no combat, random drops or navigation. | Charge at barrier arrivals, successful refills/heals, stock preference replacement, continuation work. | Motivated by Metroid; private correspondence open. |
| History maze | Equal visible locations have different futures after earlier choices; a protocol requiring a prior handshake sequence. | Bounded raw choice history is observable in the sufficient representation; no spatial movement or game-specific RAM abstraction. | Correct/wrong history arrivals, loop resets, distinct versus colliding keys. | Motivated by historical SMB 7-4 traces; fresh local correspondence open. |
| Changing actions | An action useful in one regime stops producing progress; a service changing supported operations. | Land/water/land stages, a fixed action alphabet, optional observable regime; no inertia, hazards or learned controller. | Attempts and advances by regime/action, entry to the changed regime, recovery in the final land segment. | Exploratory; reported SMB water failure lacks a verified local fixture. |

All worlds enumerate bounded reachability independently of campaign sampling.
The maze stores raw action history; the pattern is used only by the world and
post-run evaluator. Reversed mappings test physical action relabeling. The action
world's unobserved regime deliberately makes distinct states share a key. Its
normal arm uses the existing half-biased input draw; the broken arm freezes the
land action. This establishes the need for continued action exploration, not an
optimal adaptation algorithm. Resource and maze controls alter only representation.

The shared request now carries a tagged `config` with `family` and `parameters`,
and `broken` selects the family's documented control. Every nested parameter is
required and unknown fields are rejected. No compatibility path is retained for
the initial resource-only request. Diagnostic observations never supply paths or
scoring hints to selection. Action diagnostic counts exclude already-completed
parents; continuation jobs/work are reconstructed from the event stream.
