# Dissonance tiny-world evaluation plan

Status: M1–M3 completed; M4 final review and handoff in progress,
2026-09-24. See CALIBRATION.md and RESULTS.md for measured evidence and limits.

## Outcome and scope

Build a fast public suite that runs the real Dissonance search engine against
small deterministic worlds, detects meaningful regressions, and explains their
mechanisms. Calibrate game-inspired worlds privately on this laptop. Establish
five strong families before expanding the catalog. Passing the suite supplies
evidence about search mechanisms; fresh full-game runs remain the test of
end-to-end game performance.

Deliver one pull request with a semantic commit per milestone. Preserve unrelated
work. Do not combine suite development with speculative search-policy tuning.
Record discovered engine defects and additional worlds as GitHub follow-up
issues; necessary engine changes must be identified explicitly before expanding
scope. Do not maintain alternate production searchers for benchmark controls.

## Execution ownership

- Driver: `gpt-6-astra`, reasoning effort `low` (the requested light worker).
  Own the plan, interfaces, budgets, integration, experiment registration,
  interpretation, and final acceptance.
- Workers: `gpt-6-luna`, reasoning effort `xhigh`. At most three workers alongside
  the driver. Delegate bounded tasks with explicit owned files, inputs,
  deliverables, acceptance checks, and resource reservations. Avoid nested
  delegation and overlapping edits to shared interfaces.
- Freeze the small workload/runner contract before parallel world implementation.
  Give workers compact task packets rather than the entire research history.
  Workers return evidence and limitations, not just a passing-test claim.
- Only the driver authorizes builds and experiments through the shared resource
  budget. Parallel agent reasoning does not authorize parallel unbounded runs.
- Once implementation is complete and pushed, follow the shipping-code skill
  for review and CI. Follow repository Rust and README conventions throughout.

The current tool catalog supports these model/effort choices. Explicit worker
configuration follows the documented [subagent workflow](https://learn.chatgpt.com/docs/agent-configuration/subagents).
This plan does not itself switch the current task's model or start a new task.

## Laptop resource contract

All limits are aggregate across the driver, workers, builds, tests, calibration
runs, child processes, and retained task artifacts. Use decimal GB for limits.

| Resource | User ceiling | Operating policy |
| --- | --- | --- |
| CPU | 8 cores | A shared pool of eight CPU slots; explicitly bound compiler, test, emulator, and other internal worker pools. |
| Memory | 48 GB | Plan allocations below 40 GB, including measured process overhead; stop new admissions and terminate owned experiments conservatively before 44 GB or earlier under host pressure. |
| Disk | 100 GB | Normal target below 80 GB; stop artifact-producing work and clean eligible task-owned outputs before 90 GB. |

Inventory available RAM, free disk, task-owned outputs, and existing laptop load
before runs. Lower allocations when host headroom is insufficient. Include build
caches, executable versions, traces, snapshots, recordings, films, and temporary
files in disk accounting; do not duplicate private assets per worker. Build one
revision at a time, reuse compatible build artifacts, and bound retained builds.

Use conservative admission reservations plus process-tree memory and artifact
monitoring. Reservations and sampled RSS are not hard OS enforcement: before long
runs, verify bounded allocation/output behavior, headroom for in-flight work,
watchdog cleanup, and platform enforcement options. Do not claim a strict cap
from a sampling loop alone. Reduce concurrency or case size when bounds cannot
be established. A resource-stopped run is incomplete, never a search failure.

Keep compact summaries, identities, minimal witnesses, and selected failure
traces. Full streams and films are opt-in, capped, and short-lived. Delete only
explicitly inventoried task-owned disposable outputs after retaining evidence;
never clean unrelated work to make room. Record peak resource measurements.

## First five world families

Each world has independent mechanics, a declared search representation, an
objective, and bounded parameters. An evaluator can inspect full state; search
receives only the workload contract. Do not leak oracle paths into search.

| Family | Required cases and diagnostic signal | Private calibration |
| --- | --- | --- |
| Resource barrier and replenishment | A five-charge barrier; useful and unnecessary replenishment; route consumption; a competing health requirement. Measure stocked-state retention, arrival resources, continuation propagation, and escape from farming. | Metroid missile door and replenishment behavior. |
| Changing action effectiveness | Land-to-water and land-water-land analogues, with observable context and a separate unsignaled-change variant. Measure adaptation work, continued exploration, and recovery of previously useful behavior. | SMB transition into water; user's reported failure needs a verified local fixture. |
| Deadline corridor | Fast and slow arrivals at shared locations; multiple routes and a late obstacle. Distinguish remaining environmental time from search work. Measure arrival replacement and completion before the deadline. | SMB 8-1. |
| History-dependent maze | Correct versus incorrect earlier choices at similar locations; sufficient and intentionally lossy representations; both arrival orders. Measure retained useful histories and successful loop exit. | SMB 7-4. |
| Delayed progress among distractions | Unrewarded action sequences or waiting, many competing states, and adjustable required horizon. Measure useful-parent access, attempts reaching sufficient duration, and goal discovery. | Initially exploratory; use verified MM2 transition evidence if suitable local fixtures exist. |

Within each family, preserve a tiny solvable case, a representative stress case,
and a difficulty sweep. Exhaustively check reachability for bounded small
instances. The oracle verifies mechanics and reachability, not an expectation
that an uninformed explorer matches an omniscient shortest path.

## Evidence and calibration contract

Every family records its motivating failure, software analogue, assumptions,
relevant omitted details, expected failure signature, and evidence status:
exploratory, motivated, or calibrated. Synthetic success alone cannot promote
a family to calibrated.

Validate fixture schemas strictly. Include negative cases for missing, mistyped,
and unconsumed fields, and targeted semantic checks showing that the field named
by a fixture actually drives the transition or representation under test.

For game-derived claims:

1. Locate locally available licensed assets and real discovered roots. Record
   hashes, source revision, adapter identity, seed, origin, and work budgets.
   Verify native execution on the laptop; existing Linux-only tooling may need
   a bounded portability change. No remote compute is assumed.
2. Verify the actual decoded event and its representation against a short real
   trace. Avoid imagined inputs: the historical column fixture used boss health
   while the actual Zebetite event used a separate column-damage field.
3. Reproduce the smallest real failure with an appropriate comparison. Preserve
   a populated archive when competition or history matters; an isolated target
   is adequate only for local mechanics. Include more than one starting state
   where available and distinguish navigation from an in-room challenge.
4. Transfer the causal mechanism to the tiny world. Compare a known broken
   variant and a corrected variant, isolating one factor where possible. Use
   historical builds or test-only controls, not production compatibility paths.
5. Confirm the same directional effect and explanatory signature on both sides,
   across predeclared seeds. If controls change several factors, label the result
   diagnostic rather than causal. A matching score alone is insufficient.

Keep ROMs, emulator snapshots, game recordings, and private paths out of public
artifacts. Public evidence consists of original tiny worlds, constructed adapter
inputs, portable metadata, and aggregate measurements that need no private asset.
Missing assets or inconclusive evidence leave the relevant calibration open;
finish independent public work but do not claim that calibration milestone done.

## Measurements and acceptance

- Correctness: snapshot/restore consistency, deterministic fixed-work reruns,
  replay where supported, objective validity, state retention, accounting, and
  independently checked scoring. Test held-at-root objectives and delayed first
  observations; the existing ladder had errors in both areas.
- Search quality: success probability at fixed logical work; work-to-objective
  curves including censored runs; per-family regressions; uncertainty over seeds
  and instances. Never average only successful runs or equate job counts with
  work when suffix lengths differ.
- Explanation: attempted selections from useful states, useful states lost by
  retention, resource levels at arrival, useful transitions, continuation cost,
  and action-strategy work. Diagnostics must not influence search decisions.
- Practical cost: elapsed time, logical work, peak memory, and disk. Freeze the
  seed panel, starting states, logical admission window, and budgets. Identical
  seeds do not imply identical actions after two policies diverge.
- Separate development instances/seeds from a frozen validation set. Include
  structural variants, not just new seeds. Register comparison endpoints and
  budgets before validation; do not repeatedly peek until a result is positive.
- Exact invariants and fixed deterministic regression fixtures may be required for PRs.
  Exploratory performance claims belong in comparative panels until a justified
  acceptance threshold is established. Do not force the current engine to pass
  every challenge by weakening the world or tuning the engine in this project.

Runtime targets, to be measured on this laptop: the initial five-family fast
panel finishes within 60 seconds after compilation; the broader synthetic panel
within five minutes. Track cold build time separately. CI gets its own measured
budget and registry entry. If targets are missed, reduce redundant instances or
optimize the harness without erasing the failure mechanism.

## Milestones and delegation

### M1 — Harness and first vertical slice

Audit and pin the actual search revision; do not assume main and the active
two-level-searcher branch have the same contracts. Use one isolated checkout for
the implementation. Inventory assets and resource headroom, implement the shared
budget supervisor, and establish the workload adapter and compact comparison
report by building the resource-barrier world through the real campaign engine.

Driver owns interfaces and integration. Luna tasks: bounded runner/accounting
implementation; independent world mechanics/oracle; read-only private fixture
and historical evidence inventory. Sequence dependent work explicitly.

Exit: reproducible end-to-end run, checked objective and accounting, meaningful
broken control, measured cost, and a named private calibration path. The resource
controls must be exercised on small jobs before expensive experiments.

### M2 — Three families, demonstrated depth

Finish resource-barrier/replenishment, history-dependent maze, and changing action
effectiveness. Delegate one family per worker after the interface stabilizes;
serialize expensive builds/calibration through the driver. Perform private
calibration for each game-derived mechanism and preserve limits of the evidence.

Exit: all three have mechanics tests, meaningful controls, stress parameters,
held-out evaluation, diagnostics, and completed local game correspondence. Make
any missing fixture or unresolved causal claim explicit rather than expanding
the catalog to avoid it. Do not add more families before this requirement is satisfied.

### M3 — Five families and selected interactions

Add deadline corridor and delayed progress with distractions. Calibrate the
deadline world against SMB 8-1. Keep delayed progress exploratory unless a real
correspondence is demonstrated. Add only two initial interactions: deadline plus
changing actions, and resource improvement carried through multiple transitions.

Exit: five independently understandable families; predictable parameter effects;
all game-calibrated claims supported; no claimed universal ranking of search
policies; measured runtime and resource targets; one useful comparative report.

### M4 — Public CI, independent review, and handoff

Wire exact checks and the bounded panel into the applicable workflow using
`scripts/ci_contract.py` and `docs/WORKFLOWS.md`. Update nearby READMEs with one
command for the fast panel, one for broader comparisons, and private calibration
instructions. Validate clean-checkout operation without game assets. Verify
formatting, lint, relevant tests, and evidence export; use Miri if unsafe logic
is introduced. Have a worker independently try to falsify the controls, scoring,
resource accounting, and calibration claims. Complete review and passing CI.

Exit: one reviewed PR containing the four milestone commits, an evidence table,
measured laptop/CI costs, and explicit limitations. No merge without authorization.

## Expansion backlog after the first five are established

Record as ranked follow-up issues, not first-pass implementation obligations:

- Necessary backtracking and momentum; monotone preferences that destroy access.
- Bounded memory, eviction, and pinned snapshots under concurrent reservations.
- Compatible versus incompatible splice donors; long expensive unproductive tails.
- Continued propagation after resource or capability upgrades, including stale
  queued improvements and resource-gaining routes.
- Alternative health/ammunition portfolios and capability identity versus count.
- Large archive fairness, changing distractor populations, and recurring regimes.
- State-label relabeling and action-order permutations that expose accidental bias.
- Source-built public game fixtures as another bridge to realistic behavior.

Promote a backlog family only when it covers a distinct failure, fits the runtime
budget, and has independently checked mechanics and a useful diagnostic control.

## Local evidence used to form this plan

- Claude session `62236a46-3b01-431d-ab5a-684fc3688bf1`, “Build the two-level
  searcher against the ladder”; inspected through 2026-09-24 02:18:07 UTC.
  The latest Mother Brain C/D comparisons were unfinished at that point.
- `benchmarks/search/exports/searcher-consolidation-plan.md`: historical build
  outcomes and revised diagnoses, not a specification of current main.
- `nice-chebyshev-1b47d1` worktree: `benchmarks/search/metroid-ladder.json`,
  `benchmarks/search/ladder.py`, and Metroid/SMB archive and decoder tests.
- Current `dissonance/searcher/README.md`, `benchmarks/search/README.md`, and
  `docs/WORKFLOWS.md`: integration and CI contracts to recheck at implementation.
