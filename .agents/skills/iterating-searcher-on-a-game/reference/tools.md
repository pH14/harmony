# Tools for game iteration

`benchmarks/search/README.md` describes the runner, manifests and assets. Each
workload's README describes its milestones, tapes, film tool and probe. This
file lists what the process uses and the details that cost time when missed.
Rooted starts, milestone tapes, named milestones, `milestones.py` and the
ladder exist for Metroid only today; another game adds them in step 1.

## Runner

- `benchmarks/search/eval.py build` builds `nes-eval` and `nes-film`. Build
  the change and its base separately. `eval.py run` runs a manifest with one
  build; both arms of a comparison use the same manifest.
- `eval.py compare` compares two run directories. Its milestone medians leave
  out runs that never reached the milestone, so compute medians with censored
  runs counted at the tries they reached. `milestones.py` prints the first try
  at each milestone. `eval.py ladder` scores rooted chains.
- Manifests with private paths, ROMs, cores and run output stay outside the
  repository.
- Set `frames` above `executions` times the frames per try, or the frame
  budget ends the run first.
- Raise `--disk-limit-gib` above its default for checkpointed runs, because
  checkpoints count toward it and the runner kills a cell over its limit.

## Checkpoints and resumes

- `checkpoint_on_progress` keeps a checkpoint at each new milestone and top
  tier; `checkpoint_every` adds interval checkpoints. They go to
  `campaign/checkpoints/` with one shared `snapshots.store`, so keep the whole
  directory. The checkpoint that opens a hard part is the first
  `-milestone.ckpt` at or after the first try of its opening milestone.
- A case with `resume` continues a checkpoint. Budgets count from power-on, so
  set `executions` to the checkpoint's execution count plus the slice length.
  Legs are tries after the resume: a milestone's first try, which counts from
  power-on, minus the checkpoint's execution count in its file name.
- A resume with the original seed repeats the original run, apart from
  timing-dependent fields. Another seed keeps the archive and draws new
  choices.
- A resume may change the searcher's policies: mixture, retention, selector,
  continuation, objective stop, draw tables and preference. Give a
  changed policy a new identifier, or the resume records no change.
- A resume refuses a changed workload identity, any other workload policy
  (such as the key), worker count, admission window, limits or checkpoint
  format. Test such a change with rooted segments and a new power-on run.
- Resumed runs use witness verification.

## Rooted segments

- A case with `root_input` starts from the state that input reaches, with an
  empty archive.
- Take roots from a finished cell's `campaign/milestone-inputs/`: `NAME.json`
  is the first arrival and `NAME-<quantity>.json` the best-stocked arrivals.
  Probe a tape to read its resources before rooting at it.
- A tape from a rooted run starts at its root. Rooting at it needs the root's
  input joined in front; probe the joined input to check it.
- `eval.py compare` refuses to combine rooted and power-on cells for one case.

## Reading a run

- `campaign/progress.jsonl` holds the progress lines. The last line's
  `workload_diagnostics.named_progress.first_seen` has each milestone's first
  try.
- `selector.draws_by_cell` gives draws per place every 100,000 tries, which is
  the data for a heatmap during a run.
- `retained_diagnostics` on a finished run's last line is the archive census
  per place. A stopped run writes no census.
- Film the witness and the milestone tapes with the game's film tool, and read
  bytes with its probe, both in `workloads/nes/src/bin`. `eval.py film` renders
  the Nova and STB benchmarks only.
- A run keeps inputs only for the witness and the milestone tapes. Filming
  other archive states needs their inputs written out.

## Regression checks

- Run `benchmarks/search/smb-regression-three.json` with the change and with
  main, then `eval.py compare` the two run directories. Every cell must solve.
- `uv run workloads/tiny-worlds/scale.py` measures slowdown, core scaling and
  memory between two builds, for the CPU and memory cost.
