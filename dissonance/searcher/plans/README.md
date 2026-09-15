# Searcher groundwork: order of work and how to check it

Four changes to the generic searcher, built one after another on one branch
each. Every step starts from the merge commit of the step before it. The step
plans in this directory carry the file paths, line numbers and tests. This
file says what order to work in, what to check after each step, and how to
read the results.

Line numbers in the step plans are from `58d07433`. Refresh them against the
step's actual base before editing.

## Order

| Step | Plan | Base | What changes | Check after |
|---|---|---|---|---|
| 1 | `01-archive-ordering-split.md` | main | the searcher stops reading key field order as progress; the class draw becomes a weight | local checks, SMB regression, quick panel |
| 2 | `02-energy-reset-depth.md` | step 1 merge | a productive draw resets energy only at the depths where the child is new | local checks, SMB regression, quick panel |
| 3 | `03-continuation-graph.md` | step 2 merge | continuation replay sized to the archive, breadth-first, on by default | local checks, SMB regression, quick panel, long panel, throughput |
| 4 | `04-input-table-in-searcher.md` | step 3 merge | the retained-input table leaves the SMB driver and every workload gets it | local checks, SMB regression with exact comparison, quick panel, long panel |

The order matters. Step 1 removes selector policy variants that step 2 would
otherwise have to handle. Steps 1 and 2 together flatten the draw over
covered places, and step 3 is what then pushes toward the frontier, so the
long panel is not run until step 3 is in. Step 4 rewrites the input policy
surface and is independent of the archive internals, so it goes last.

Each step is one pull request. Open it when the step's tests pass, push it,
and run the review described in the shipping-code skill. Merge, then start
the next step from the merge commit.

## What we expect

These are the standing expectations for the whole sequence. They are
judgements to make, never numbers to compare against.

- Super Mario Bros stays fast to solve. The SMB regression manifest keeps
  passing on every seed.
- Metroid, Mega Man 2, Nova and Super Tilt Bro get at least as far as they do
  today on the quick panel, seed noise allowed for.
- The changes stay in even when a panel shows no gain. They are the right
  structure for the searcher, and a later step may be what makes an earlier
  one visible.
- Throughput does not get worse in a hot path. Extra bookkeeping per
  admission is expected. New work that grows with the archive on a
  per-admission or per-reservation path is a defect to fix before moving on.

## How to judge a step

Look before counting. For every panel run, in this order:

1. Film the deepest witness of each Metroid and Mega Man 2 seed with
   `metroid-film` and `mm2-film`, and watch it. Write one sentence per seed
   saying where the run ended and what it was doing.
2. Render the Metroid map coverage from the end-of-run census field
   `live_entries_by_map_cell` in the run report, one image per seed, and put
   it beside the previous step's image for the same seed. If no renderer is
   on the box, write a short Python script with PEP 723 metadata under the run
   directory and keep it there.
3. Only then read the numbers: milestones reached per seed at matched
   executions, entries and draws per area, executions per second.

Rules for reading the numbers:

- Seeds differ a lot on their own. Worker count alone has flipped whether a
  Metroid seed reaches Ridley's area. One seed going backwards on one
  milestone is noise. Every seed going backwards on a milestone the previous
  step reached on every seed is a regression.
- A regression is a reason to find the cause and fix forward. It is never a
  reason to drop the step or revert to the previous step's behaviour.
- Never turn an expectation into a threshold. There is no percentage that
  passes or fails a step. If you find yourself computing one, stop and
  describe what the film and the map show instead.
- Compare at matched executions, never at matched wall time, since runs share
  the box.
- A game that solved before and does not solve now on any seed of the quick
  panel is the one case to stop on. Diagnose it before starting the next step
  and put the diagnosis in the pull request.

## The checks

Local checks, run before every push. These are what CI runs for searcher
changes in `.github/workflows/search-eval.yml` and `quality.yml`:

```sh
cargo fmt --all -- --check
cargo clippy --all-features --all-targets -- -D warnings
cargo test --manifest-path dissonance/searcher/Cargo.toml --locked
cargo test --manifest-path workloads/nes/Cargo.toml --lib --bin nes-eval --locked
python3 scripts/custom-lints.py
python3 scripts/strip-comments.py --check
python3 scripts/check-dependency-boundaries.py
python3 -m unittest discover -s benchmarks/search -p 'test_*.py'
```

Box runs, on ms02, following `benchmarks/search/README.md`. Build once per
step with `eval.py build` and keep the build directory named for the step.

- **SMB regression**: `benchmarks/search/smb-regression.json`. Five seeds,
  must solve on every seed within budget. About ten minutes.
- **Quick panel**: `benchmarks/search/pilot.json`. Three seeds, six cases,
  every game, 400,000 executions per case with a fifteen-minute wall limit.
  About two hours. This is the "at least as far as today" check.
- **Long panel**: `benchmarks/search/metroid-long-horizon.json`. Seeds 3, 4
  and 5 at 3,000,000 executions, four workers. Several hours. Run only where
  the table above says so.
- **Throughput**: executions per second and frames per second from the
  quick panel's run reports, compared with the previous step's quick panel
  on the same box. If it fell, profile one case with `perf record` on the
  box and read whether the new code is on the hot path. The size of the
  drop is not the question; where the time went is.

Keep the run directories on ms02 named by step and date. Put the numbers,
one sentence per seed from the film, and the map images in the pull request
description. Nothing from a run goes into the tree except a results file
under `benchmarks/search/results/` if the step's plan asks for one.

## Working rules

- Read `CLAUDE.md`, `REVIEWING.md` and `dissonance/searcher/README.md`
  before starting a step. Rust carries no comments. Component knowledge goes
  in the nearest README, and every step updates `dissonance/searcher/README.md`.
- Backwards compatibility is not a goal. When a step removes a policy
  identifier, recorded runs that name it fail to load with a clear error, and
  the benchmark manifests are updated in the same pull request.
- Keep each step to its plan. If a plan turns out to need something it does
  not say, write the addition into the plan file in the same pull request and
  say so in the description.
- When stuck for more than a couple of hours on one problem, stop and write
  down what was tried, what the checks say, and what the film shows. That
  report is the deliverable at that point.
