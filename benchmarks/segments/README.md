# Segment benchmark

`segment-bench` compares search arms on short, fixed segments of a game by the
work each arm needs to reach a milestone. A segment starts from a saved game
state just before a hard place. Each arm runs every segment at many seeds. The
work to the milestone is scored as survival data: a seed that runs out of
budget counts as censored at the budget. Any searcher takes part through an
adapter command, described below.

## Segment specs

Each segment is one JSON file in `specs/`:

```json
{
  "name": "smb-8-4",
  "game": "smb",
  "rom_sha256": "<64 hex>",
  "cartridge_ram": false,
  "start": {"tape": "smb-8-4.tape", "tape_sha256": "<64 hex>", "core_state_sha256": "<64 hex>"},
  "milestone": [{"address": 1904, "eq": 2}, {"address": 1887, "eq": 7}],
  "work_budget": 134217728,
  "workers": 4,
  "memory_limit": 4294967296
}
```

- `name` is lower case letters, digits and hyphens. It names the result
  directories.
- `game` is a label for tables. Game identity in the verdict is `rom_sha256`,
  the SHA-256 of the ROM file.
- `cartridge_ram` true means the ROM is loaded with the iNES battery bit
  (flags 6, bit 1) set, so `$6000`-`$7FFF` is RAM.
- `start.tape` is a path relative to the spec file. A tape holds one byte per
  frame from power-on for controller 1, in NES pad bit order: bit 0 A, 1 B,
  2 Select, 3 Start, 4 Up, 5 Down, 6 Left, 7 Right. The runner refuses a spec
  whose tape does not match `tape_sha256`.
- `start.core_state_sha256` is the SHA-256 of the QuickNES serialized state
  after the last tape frame, with the Harmony envelope removed: the bytes
  `nes_machine::nes::core_state` returns
  (`workloads/nes-machine/src/nes.rs`). The core is QuickNES built by
  `scripts/build-quicknes-core.sh`.
- `milestone` is a list of predicates on RAM bytes, all of which must hold
  after one emulated frame. `address` is a CPU address in `$0000`-`$07FF`, or
  in `$6000`-`$7FFF` when `cartridge_ram` is true. Each predicate has exactly
  one of `eq`, `ne`, `ge`, `le` (a value 0-255), `bits` (every set bit of the
  mask is set in the byte) or `bucket` W (1-255: the byte's value divided by W
  differs from the same quotient at the start state).
- `work_budget` is the work at which a seed is censored.
- `workers` is the number of workers every run of the segment uses.
- `memory_limit` is in bytes.

The seed is not in the spec. The runner adds it to each run.

## Adapter contract

An arm is a command line in a local arms file, which stays out of the
repository:

```json
{
  "control": {"command": ["/path/to/adapter", "--some-flag"]},
  "candidate": {"command": ["/path/to/adapter", "--other-flag"]}
}
```

For each run the runner appends two arguments, the path of `request.json` and
the path of `result.json`, and runs the command. The request is the spec with
`start.tape` made absolute, plus `"seed"` (an unsigned integer). The adapter
writes `result.json` before it exits.

The adapter must:

1. Verify that the SHA-256 of the ROM it loads equals `rom_sha256`, or stop
   with `rom_mismatch`.
2. Play the tape from power-on and verify that the SHA-256 of the core state
   after the last tape frame equals `start.core_state_sha256`, or stop with
   `start_mismatch`.
3. Search from that state until the milestone holds, the work reaches
   `work_budget`, or the search's memory reaches `memory_limit`, using exactly
   `workers` workers.

Work is the number of emulated frames the searcher runs from the start state,
summed over all workers and counted in the searcher's deterministic order.
Restoring a saved state costs nothing. Running inputs again to rebuild a state
counts. The adapter checks the milestone after every emulated frame. Work to
milestone counts every frame up to and including the first frame on which
every predicate holds.

The search reads the budget only to stop. A run at a larger budget therefore
gives the same result as a run at any smaller budget, up to the point where
the smaller one stops. Scoring relies on this: a result whose work is above the
spec's budget counts as censored at the spec's budget.

`result.json`:

```json
{"stop": "milestone", "work": 12345678, "job_stream_sha256": "<64 hex>", "message": ""}
```

- `stop` is one of `milestone`, `budget`, `memory`, `start_mismatch`,
  `rom_mismatch` or `error`. The run reached the milestone when `stop` is
  `milestone`. `budget` and `memory` are censored at their `work`.
  `start_mismatch`, `rom_mismatch` and `error` are failures: they are left out
  of scoring and listed in the score output.
- `work` is the work as defined above.
- `job_stream_sha256` is the SHA-256 of the searcher's deterministic decision
  stream. The same request gives the same hash on any host, and should give
  it at any worker count.
- `message` is free text for failures.

An adapter that exits without a readable `result.json` is recorded as
`error`.

## Commands

```
segment-bench run --specs <file or dir> --arms <arms.json> --arm NAME [--arm NAME ...] [--seeds 1-16] [--jobs N] --out DIR
segment-bench score --specs <file or dir> --out DIR --control NAME
segment-bench calibrate --specs <file or dir> --out DIR --control NAME
segment-bench determinism --specs <file> --arms <arms.json> --arm NAME [--seed S] [--other-workers W] --out DIR
```

`run` writes `DIR/<segment>/<arm>/s<seed>/` with `request.json`,
`result.json`, `log` (the adapter's output) and `run.json` (wall time and exit
status). It runs `--jobs` adapters at once. A run whose stored request equals
the new one and whose result is valid and not `error` is skipped, so an
interrupted `run` resumes, and a spec change runs the affected seeds again.
`--seeds` takes ranges and lists such as `1-16` or `1-4,9`.

`determinism` runs one segment and seed twice, first at the spec's workers and
then at `--other-workers` (default half the spec's workers, or 2 when the spec
has 1), and passes when stop, work and `job_stream_sha256` agree. Run it on
each host before counting a table.

## Scoring and verdict

`score` prints one Markdown table line per segment and arm and writes
`DIR/score.json`. A line holds:

- reached k/n: seeds that reached the milestone within the spec's budget.
- The Kaplan-Meier median work to milestone. When fewer than half of the seeds
  reach it, the median is undefined and shown as above the budget.
- A 95% bootstrap percentile interval for that median: 2,000 resamples of
  seeds from a ChaCha generator with a fixed seed per segment, so the table is
  the same on every run. A resample with no median counts as infinite, so an
  interval bound can be infinite.
- The log-rank p-value against the control arm (chi-square with one degree of
  freedom).
- The median wall time per seed.

An arm passes when its interval lies entirely below the control's on at least
two segments from different games (different `rom_sha256`) and entirely above
the control's on none. Any other arm fails. `score` prints each arm's verdict
with the segments behind it.

Sixteen seeds resolve only large differences. With work to milestone drawn
from an exponential distribution, the 95% interval of a 16-seed median spans
about a factor of four, so even a 2.5-fold difference in work can leave the
intervals overlapping. The synthetic test uses 64 seeds for that reason.

## Calibration

`calibrate` prints, for each segment, the control's reached k/n at the spec's
budget, a suggested budget (the control's Kaplan-Meier median rounded up to a
multiple of 2^20) and a refit flag. A budget should let the control reach the
milestone on about half of the seeds. 0/16 or 16/16 means the segment needs a
new budget or a new milestone. Calibrate from control runs at a generous
budget. Because the search reads the budget only to stop, those runs score
correctly at the smaller budget.

## Choosing a segment

A segment starts where control runs spent the most work per milestone: the
place a whole-game run takes longest to get through. The start state is a kept
state from such a run just before that place, and the milestone is the first
RAM change that shows the place is behind.

## Repository rules

Specs reference ROMs by hash. ROMs, arms files, run records and result tables
stay out of the repository.

## Tests

```
cargo test -p segment-bench
```

The tests need no ROM. `segment-bench stub-adapter` is a hidden adapter that
draws work from an exponential distribution seeded by the segment name and
seed, scaled by `--factor`, with a job stream hash keyed by `--stream-key`.
Three synthetic segments across two fake games in `tests/specs` check that a
faster arm passes, a slower arm fails with regressions and an arm equal to the
control fails with none.
