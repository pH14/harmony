<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Dissonance v4

Dissonance v4 is one state-exploration engine for deterministic targets. It
drives an NES game from power-on and a replicated system under fault
injection through the same adapter contract. It never sees a name, an
address, an assertion message or a target identity.

| crate | role |
|---|---|
| `engine` | target-neutral search: archive, selection, job generation, deterministic batches |
| `nes` | NES adapter over `workloads/nes-machine` (QuickNES), the `segment-bench` adapter command, power-on runs and route replay |
| `faults` | fault-injection adapter over `workloads/faults` for replicated systems in a deterministic VM; see `faults/README.md` |

## Adapter contract

Per observation the adapter hands the engine:

- a fixed vector of unnamed `u64` scalars, each with a declared kind:
  `identity` (explore every distinct value), `higher` or `lower` (progress:
  prefer states that push the value further in that direction);
- one unnamed byte array.

Actions come from a `Vocabulary`: a count of input kinds, whether an input can
be held over several steps, and a base job length in steps. An `Action` is an
input index, a random `u64` argument the adapter uses for its own parameter
draws, and a hold. A `Worker` restores an opaque snapshot, runs actions step
by step, reports each step's observation to a `Probe`, and captures a snapshot
when the probe asks for one. Work is counted in steps (emulated frames for
NES). Restoring a snapshot costs nothing.

## Engine

- **Cell.** Cells have two levels. A coarse cell is the identity scalars,
  each shifted right by `w_i`. `w_i` comes from how far the search's own jobs
  move that scalar: with the median per-job span in `[2^(b-1), 2^b)`,
  `w_i = b - 1`, so a typical job crosses one or two coarse buckets of that
  scalar. A scalar the jobs rarely move stays exact. A fine cell shifts each
  identity scalar by `w_i - 2` (floored at 0), so it is a quarter of the coarse
  width. States are kept per fine cell; selection weights coarse cells. A
  typical job moves a scalar further than the gap between places that matter
  (a platform and the ground below it), so a coarse cell alone would keep one
  of them. A job's span counts only steps no larger than the step limit,
  `2^(m+1) - 1` where `m` is the median log2 of that scalar's nonzero
  per-step changes; a larger step (a respawn, a room wrap, a teleport) ends a
  stretch, and the span is the widest stretch. Widths and step limits are
  recomputed from the job stream every 16 batches and the archive is re-keyed
  when the widths change.
- **Retention.** Each fine cell keeps a Pareto front over the progress scalars. A
  new state enters when no holder is at least as good on every progress
  scalar; equal progress keeps the shorter route from the start. Dominated
  holders drop their snapshots. Route records are kept for holders and their
  ancestors; the rest are cleared whenever the states added since the last
  clearing outnumber the states it kept.
- **Selection.** A coarse cell is drawn with weight
  `1 / (sqrt(1 + n) * (1 + r))`, then one of its fine cells with the same
  formula over that fine cell's own counts. `n` counts the times the cell was
  chosen since a front in it last changed; an improved arrival resets it, so
  the improvement is explored forward and replaces downstream holders it
  dominates. `r` is the cell's best rank over the progress scalars: for each
  progress scalar, the number of distinct better values any holder has,
  minimised over scalars and over the fronts in the cell. Ranks are refreshed
  every 16 batches; a new cell is ranked on arrival. A member of the front is
  drawn uniformly.
- **Jobs.** Inputs are drawn uniformly; holds are log-uniform from 1 to 63
  steps. A job from a cell runs `base * 2^L(e + 1)` steps, where `L` is the
  exponent of the Luby restart sequence (1, 1, 2, 1, 1, 2, 4, ...) and `e`
  counts consecutive jobs from that cell that admitted nothing, so short jobs
  keep recurring while the longest length grows without bound. A re-key gives
  each new cell the largest `e` of the old cells merged into it, so widths
  that change often do not hold every job at the base length.
- **Random arm.** `root_only` takes every job from the start state at the base
  length, with the same executor and accounting.
- **Determinism.** Jobs are planned and admitted in fixed batches of 32 in
  job order, so the job stream does not depend on the worker count. The
  stream hash covers seed, kinds, every job's parent and actions, admissions
  and re-keys. The budget is read only to stop.
- **Memory.** Snapshots are stored LZ4-compressed.

The archive report (`cells.jsonl`, `edges.json`) lists every cell with its
front, first discovery, choice counts and the cell it was first reached from.

## NES adapter

`nes/games/*.json` declares, per ROM, the scalars (CPU addresses, low byte
first; `$6000`-`$7FFF` is cartridge RAM), their kinds, the controller bits the
chord inputs cover, whether one extra input taps Start, and the goal
predicate used for power-on runs. The goal and segment milestones stop a run;
no search decision reads them.

Preparation rule used for the practice games:

- Identity scalars are places and permanent capabilities: area, room, screen,
  coarse position, the counters a loop castle uses to send the player back,
  equipment, boss flags and items.
- Progress scalars are resources and counters the game waits on: health,
  missiles, lives, barrier and boss hits, door hits remaining.
- A level position or level number the game only moves forward through
  (Famidash's scroll, SMB's world and level) is declared twice: as identity,
  so the level is split into places, and as higher, so each place keeps its
  furthest state.
- A frame counter that hazards cycle on (SMB's `$09`) and the coarse game
  clock are identity, so the archive keeps arrivals at one place in different
  phases. Vertical velocity is identity when the player cannot stop moving
  (Famidash's flying modes).
- Posture, animation and scroll bytes are left out.
- A game may declare `over` predicates. A job stops at the first frame
  where they all hold, so the engine never sees a state after a game over.
  Ice Climber declares one because its title screen sets the mountain
  counter directly.

A multi-byte scalar marked `signed` is read as two's complement and stored
with its sign bit flipped, which keeps numeric order.

```
v4-nes bench --host HOST.json REQUEST.json RESULT.json
v4-nes run --host HOST.json --game NAME [--tape T] --seed S --workers W --budget N --out DIR [--root-only] [--memory-limit BYTES]
v4-nes replay --host HOST.json --route DIR/route.json [--tape T]
```

`HOST.json` holds `core` (the QuickNES core path) and `roms` (ROM SHA-256 to
path). `bench` is the `segment-bench` adapter command. `run` writes
`progress.jsonl`, `cells.jsonl`, `edges.json`, `result.json` and, when the goal
holds, `route.json` with the button chords from power-on. `replay` plays a
route on a fresh core and reports whether the goal holds.

## Tests

```
cargo test --manifest-path dissonance-v4/Cargo.toml
```

The engine tests need no ROM. A toy corridor has a door that needs six ammo
picked up far behind it; the test checks that the engine carries ammo beside
the position cell to the door, that the route replays, that one and three
workers give the same job stream, and that a smaller budget is a prefix of a
larger one.
