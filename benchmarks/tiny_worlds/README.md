<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Dissonance tiny-world evaluation

The [approved plan](PLAN.md) separates exact engine/workload invariants from
statistical search quality and private game correspondence. The implementation
contains resource, history, changing-action, deadline, and delayed-progress worlds,
plus deadline/action and multi-transition resource interactions. The first-three-
family mechanism requirement was completed before that expansion.
[RESULTS.md](RESULTS.md) records the 546 verified synthetic runs, held-out
comparisons, negative results, runtime, and resource measurements.

`panel.json` registers development and held-out structural instances and seeds
before evaluation. The endpoint is a valid objective witness within the admitted
logical-work budget. Every unsolved case remains censored at that budget; an
infrastructure/resource stop is incomplete and never a search failure. Same seeds
do not imply matched actions after representations diverge. Do not repeatedly
inspect held-out results while changing policies or thresholds.

Use one private output parent for every task job so supervisors share the same
reservation ledger. Explicitly inventory build, cache, temporary, and result
roots; no licensed asset needs to be copied. For example, after creating
`/tmp/tiny-worlds` and `workloads/tiny-worlds/target`:

```sh
python3 benchmarks/tiny_worlds/supervise.py --out /tmp/tiny-worlds/build \
  --artifact-root workloads/tiny-worlds --artifact-root /tmp/tiny-worlds \
  --cpu-slots 1 --memory-gb 2 --disk-gb 2 --seconds 180 \
  -- cargo build --locked --release --manifest-path workloads/tiny-worlds/Cargo.toml -j 1
python3 benchmarks/tiny_worlds/supervise.py --out /tmp/tiny-worlds/development \
  --artifact-root workloads/tiny-worlds --artifact-root /tmp/tiny-worlds \
  --cpu-slots 1 --memory-gb 1 --disk-gb 0.1 --seconds 60 \
  -- python3 benchmarks/tiny_worlds/panel.py \
  --binary workloads/tiny-worlds/target/release/tiny-worlds
```

Point temporary files and language caches at inventoried task directories, or
include their existing roots with additional `--artifact-root` arguments. Add
`--split validation` only after freezing the implementation for a held-out run.
Each output directory must be new. `stdout.log` holds the compact panel report;
`summary.json` holds resource measurements and completion status. Preserve both.
The source registration pins the actual engine bytes, not just a branch label.

The supervisor reserves at most eight CPU slots and keeps memory/disk
reservations below 40/80 decimal GB. It refuses low host headroom and stops owned
process groups on wall, sampled memory, or sampled artifact-growth limits. Output
logs and telemetry are bounded; child files are limited to 256 MiB and core dumps
are disabled. Linux also applies CPU affinity. macOS relies on explicit compiler,
search-worker, and library thread limits. RSS and aggregate disk checks are sampled
and are not strict OS caps. Use trusted non-daemonizing commands: a child that
creates a separate session is outside process-group containment. Never use this
wrapper to justify an otherwise unbounded long run. Resource stops are incomplete.

The resource representation control keeps mechanics, engine, seeds, budget,
window, actions, and preference count fixed, but zeros the key's charge field.
It asks whether useful stocked states survive. Route consumption and health
requirements provide structural variation. Search never receives the reachability
oracle's paths. Output includes per-seed logical work, objective censoring,
arrival-charge observations, retained states, selection/portfolio counters,
stream digest, elapsed search time, and logical memory. Resource supervisor
measurements cover verification and child processes too.

## Private calibration requirement

Use existing local licensed assets in place, with hashes and immutable build
identity. Never copy ROMs, snapshots, films, recordings, private paths, or full
traces into this directory. Keep detailed run records outside the source tree;
only portable aggregate evidence and experiment metadata belong in public reports. Record actual event decoding before making a causal
claim: Metroid Zebetite column damage is separate from boss health, and SMB 7-4
requires earlier correct-check history. Historical reports are leads, not fresh
calibration of the pinned engine.

Local native comparisons now establish the first three families' mechanism
correspondence: resource retention through a verified five-missile door, useful
history retention in SMB 7-4, and continued button exploration in water. See
[CALIBRATION.md](CALIBRATION.md) for controls, counts, negative results, and limits.
The retention experiments use actual populated archives and known diagnostic
continuations; they do not establish autonomous route discovery or full-game
gains. Native calibration tools are documented in `workloads/nes/README.md`;
no guest image is required. Follow-up evidence is tracked in
[#393](https://github.com/pH14/harmony/issues/393), and post-five-family expansion
in [#394](https://github.com/pH14/harmony/issues/394).

## Three-family registration

M2 adds history and changing-action families before any five-family expansion.
The history control omits raw prior choices from the archive identity; both
arms execute the same maze. The action control freezes the starting land action
while the normal arm continues sampling the full alphabet. Both action arms use
the engine's existing `biased_half` mixture to exercise retained-action bias;
resource and maze arms keep `alphabet_only`. These are registered workload
conditions, not engine-policy changes. The unsignaled action variant intentionally
aliases regimes and may defeat both arms. Success on the observable variant does
not establish learned adaptation or water-level game performance.

The resource extension lets refill consume health and lets a station visit
restore health by spending a charge. It tests whether two useful resource
portfolios survive and propagate along a consuming route. Configuration fields
are required and bounded; zero refill health cost preserves the first slice.

After an intentional workload change, run `python3 benchmarks/tiny_worlds/register.py`
once before the corresponding validation. It updates only the workload source
hash and refuses engine drift. It never changes seeds, instances, budgets or
endpoints. CI uses `register.py --check` to verify both source identities without
re-registering them. The held-out set remains untouched
until the implementation is frozen; development seeds become regression seeds
once observed.

Logical-work budgets stop new admissions; the final admitted job can cross the
threshold. One reservation and the 128-action path cap bound that overshoot to
127 transitions, which the harness checks and reports separately. Objectives
first observed beyond the threshold are retained as evidence but scored censored.
The suite does not demand every normal arm solve every stress/aliased instance.

For the broader registered difficulty/work sweep, use the same supervisor command
with a fresh output directory, `--seconds 300`, and `panel.py --split sweep`.
It uses development seeds only, three logical-work budgets, and predeclared route,
history-length, and regime-length variations. Each report retains per-seed event
and censoring data; a conditional average over successful runs is never the score.

## Five-family registration

The deadline family gives a one-action slow route and a two-action fast route
different environmental durations, followed by a late obstacle. The normal key
prefers remaining time at shared positions; its control omits that value. Search
work still counts every transition once, regardless of its environmental duration.
The native 8-1 diagnostic uses a distinct reversed-clock-preference control,
which tests ranking direction without erasing clock information.

Delayed progress remains exploratory. Its sequence and waiting variants require
consecutive useful actions among distractor states, with no objective progress
before the threshold. The control drops partial progress from archive identity.
The deadline/action product charges time for ineffective actions too; its control
freezes the starting action while preserving the timer representation. The
resource interaction uses the existing refill and consumption mechanics across
four or five transitions, with arrival charge and continuation work reported.

Development and validation run at 4,000 logical transitions per seed and arm.
The broader sweep varies deadline length, delayed horizon, and interaction length
as well as the original three families. Search quality remains descriptive;
exact replay, accounting, objective validity and registered source identity are
the required checks. Unsolved stress cases are retained rather than weakened.
