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
`omit_stock`, and `verify`. It validates the world and independently enumerates
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

Exact correctness checks gate changes. Seed-panel solve fractions, Wilson
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
