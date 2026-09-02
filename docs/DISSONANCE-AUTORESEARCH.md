# Dissonance autoresearch program

One agent, one metric, one loop. Edit the searcher, evaluate, keep or discard,
log one line, repeat. The loop never waits for a human.

## Goal

A single generic search algorithm completes Super Mario Bros. end to end. The
searcher reads mechanical observations (position buckets, engine state bytes,
level tuple) through the machine adapter and knows nothing about routes,
levels, water, or enemies. Game knowledge lives only in the adapter's byte
offsets. A change that helps one level by naming that level is invalid.

## Metric

The wall metric is the first level that no run from the current searcher has
completed from its fixture within budget. Today that is 2-2. When the searcher
completes it, the wall moves to the next level with a fixture (4-2, 4-3, 7-2,
7-4, 8-1, 8-4) and the loop continues with the same rules.

One evaluation:

- origin: the wall's fixture checkpoint; seed 20260905; 8 workers;
  60,000 executions; action limit 8192; everything else default;
- score: executions to exit the level, lower is better. Without an exit, the
  score is the final progress watermark, higher is better, and any exit beats
  any watermark;
- wall time is recorded beside the score and a run is killed at 10 minutes.

A candidate is kept when its score beats the current best. A keeper is then
confirmed on seeds 20260906 and 20260907 and must beat the best on at least
one of them and lose on neither. A keeper also runs the floor: genesis seed
20260905, 8 workers, 45,000 executions must still reach 2-1. The floor is a
floor; 1-1 speed may drop.

Determinism is a CI test, the paired same-seed stream comparison already in
the searcher's test suite. A candidate that fails the test suite is discarded.

## Progress record

Fewest executions to each milestone on seed 20260905 at 8 workers from genesis:
1-2 by 1,873; 1-3 by 5,810; 1-4 by 18,870; 2-1 by 21,345; 2-2 by 31,367;
2-3 by 38,588; 2-4 by 44,462.
The floor run updates this record. A drop below the record by more than the
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
