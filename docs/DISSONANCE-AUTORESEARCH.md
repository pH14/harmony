# Dissonance autoresearch program

One agent, one metric, one loop. Edit the searcher, evaluate, keep or discard,
log one line, repeat. The loop never waits for a human.

## Goal

A single generic search algorithm completes Super Mario Bros. end to end. The
searcher reads mechanical observations (position buckets, level tuple, level
clock) through the machine adapter and knows nothing about routes, levels,
water, or enemies. Game knowledge lives only in the adapter's byte
offsets. A change that helps one level by naming that level is invalid.

## Metric

The searcher completed the game from power-on on 2026-09-02 (keeper 6fe85d12:
241,103 / 207,014 / 259,173 executions on the three seeds, with a hand-picked
maze-loop key that has since been reverted). The metric is executions to
complete the game from power-on.

One evaluation:

- origin genesis; seed 20260905; 8 workers; action limit 8192; everything else
  default; killed at 10 minutes of wall time;
- score: executions to the victory event, lower is better. A run that does not
  complete scores by deepest level reached, then watermark, and any completion
  beats any watermark;
- wall time is recorded beside the score.

A candidate is kept when its score beats the current best. A keeper is then
confirmed on seeds 20260906 and 20260907, judged by the three-seed median, and
no single seed may lose by more than a third. Beside every keeper's score the
same run at `--memory-budget-mib 256` is recorded; a keeper may not make that
score worse than the current best's.

The level fixtures under the evaluator's e07/fixtures directory (2-2, 4-3,
7-4, 8-4) remain available as diagnostics for a stall inside one level. They
never decide a keeper.

Determinism is a CI test, the paired same-seed stream comparison already in
the searcher's test suite. A candidate that fails the test suite is discarded.

## Progress record

Fewest executions to each milestone on seed 20260905 at 8 workers from genesis:
1-2 by 1,873; 1-3 by 5,420; 1-4 by 9,672; 2-1 by 10,718; 2-2 by 16,422;
2-3 by 21,161; 2-4 by 28,430; 3-1 by 33,278; 3-2 by 35,898; 3-3 by 37,500;
3-4 by 43,467.
The scored run updates this record. A drop below the record by more than the
seed spread means a regression outside the candidate; bisect with one 45,000
execution genesis run per commit before continuing.

Anything that changes how stale the archive is when a parent is selected
(in-flight reservation depth, prefetch, admission order) is a search change and
is scored like every other candidate. Throughput never justifies a lower score.

## Loop

1. Pick the next hypothesis from the menu below or from the last result.
2. Implement it in the smallest diff that tests the idea.
3. Run the test suite, then the evaluation.
4. Keep or discard. Append one line to `dissonance/AUTORESEARCH-RESULTS.tsv`:
   commit, hypothesis in one sentence, score, wall seconds, keep or discard,
   one-sentence diagnosis.
5. Commit keepers on the program branch. Discard losers with `git checkout`.
6. Repeat. Wake at most every ten minutes while a run is in flight.

Prefer the smaller diff at equal score. Every fifth keeper, spend one
iteration removing machinery that the keepers made redundant.

## Hypothesis menu

Each line is a mechanism with its source and the failure it targets. All are
generic, deterministic, and read only archive counters or RAM bytes.

- **Adaptive mutator mixture.** Update the chord-table versus splice weight from
  which mutator produced the last new cells, per level area (MOpt, AFL++
  schedules). Targets a fixed 6:1 mixture tuned on ground levels.
- **Bandit selection over cells.** Allocate selections by measured new-cells
  per execution with a count-based prior for cold cells, replacing fixed retire
  thresholds (AFLFast, EcoFuzz, Go-Explore count weights). Targets early
  retirement in areas where rollouts are noisy.
- **Rollout length as an arm.** Make the suffix length per area a bandit arm
  instead of the fixed one-to-six ("When to Go, and When to Explore"). Targets
  momentum-driven areas that need long or short holds.
- **Inferred key bytes.** Score RAM bytes by few distinct values, low change
  rate along a lineage, and separation of productive from barren parents;
  build the cell key per area from the top bytes and drop the hash bits
  (stateful greybox fuzzing state inference; Go-Explore cell adaptation).
  Targets merged states with different momentum and split states that differ
  only in timers.
- **Adaptive cell resolution.** Split a cell's key finer when it is over-visited
  and barren, coarsen when sparse (Go-Explore online downscale adaptation).
- **Fixed total in-flight depth.** Bound selection staleness by a constant
  number of jobs independent of worker count. Targets the 12-worker lag.
- **Portfolio over one archive.** Run several selector configurations against
  the shared archive and shift execution budget by measured yield (ensemble
  fuzzing). Targets one fixed policy for every area.
- **Connectivity graph with transition costs.** Keep best-known cost per cell
  transition and propagate improvements; give the selector distance-to-frontier
  (Antithesis Metroid). Targets long routes that exhaust the action budget.

Check the cheapest evidence first. Before building the key or resolution
candidates, group the wall fixture's recorded archive by current key and
measure within-group spread of candidate bytes and their correlation with
productive descendants; that takes minutes on artifacts already on disk.

## Boundary

Generic search modules import no target adapter. Target modules implement no
search policy. `grep` for level numbers, world numbers, or area identifiers in
`dissonance/searcher/src/search/` must return nothing.

The observations the adapter exposes are fixed: world, level, horizontal
progress, vertical bucket, death, area identity, level clock, and victory.
Engine state was removed because the fingerprint bits separate every
transition it did. Adding a RAM address by hand because a level needs it is
game knowledge and is reverted; the maze-loop check bytes were removed for
this reason. The searcher may still derive key bytes from RAM by a generic
inference that scores every byte the same way in every area.

The controller vocabulary is every physically pressable button combination
except Start, Select, and opposing directions. It is never curated. Which
combinations matter is for the chord table to learn.
