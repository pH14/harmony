<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Dissonance tiny-world evaluation

The [approved plan](PLAN.md) separates exact engine/workload invariants from
statistical search quality and private game correspondence. The implementation
starts with a resource-barrier vertical slice. The three-family calibration gate
must pass before adding deadline and delayed-progress families.

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

## Private calibration gate

Use existing local licensed assets in place, with hashes and immutable build
identity. Never copy ROMs, snapshots, films, recordings, private paths, or full
traces into this directory. Keep aggregate evidence outside the source tree;
run records are not source. Record actual event decoding before making a causal
claim: Metroid Zebetite column damage is separate from boss health, and SMB 7-4
requires earlier correct-check history. Historical reports are leads, not fresh
calibration of the pinned engine.

The resource barrier's correspondence to a missile door remains open until a
local native run verifies resource consumption/replenishment and a meaningful
comparison at a real root. Maze and water-transition fixtures also need verified
local roots and traces. Missing assets or unsupported native execution blocks
that correspondence, not independent public harness work.
