<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Autonomous Mega Man 2 campaign

Deadline: September 20, 2026 at 08:00 America/New_York (12:00 UTC).

Success means a campaign beginning from a recorded power-on boot prelude,
choosing its own stages and inputs, and reaching the ending. Its final tape
must replay continuously from power-on without intermediate restores. The
prior assisted completion is diagnostic evidence, not an autonomous result.

## Ownership and evidence

The adapter owns truthful RAM decoding, controller semantics, scene identity,
mechanical capabilities, game outcomes, and documented evaluation contracts.
The generic searcher owns selection, retention, rollout horizons, mutation,
continuation discovery, recording, and replay. No chosen obstacle weapon,
winning controller bank, manually selected intermediate root, imported solution
tape, or automatic gameplay transition is permitted in a qualifying campaign.
Rooted diagnostic probes are labelled separately and cannot establish success.

One PR will contain semantic commits separating adapter work, core searcher
work, and experiment evidence. Existing assisted evidence and PR 362 are kept.

## Milestones

1. Provide whole-game execution with recorded actions through stage selection,
   deaths, Continue, stage awards, and the ending. Remove obstacle-specific
   ammunition identity from the qualifying key and distinguish actual inventory
   identities. Check snapshot restoration and continuous frame accounting.
2. Reproduce the generic same-key waiting failure. Test a bounded generic
   remedy, including replay and negative cases, before using it in a game run.
3. Run fresh power-on campaigns on msr1 and at most eight Mac cores. Inspect
   archive support, actual continuation outcomes, and subagent-reviewed film;
   change mechanisms only in response to measured failures.
4. Verify a discovered ending on both hosts with one uninterrupted controller
   tape, publish hashes and evidence, and report any unresolved limitation.

## Initial findings

The prior target ends at the first boss and automatically supplies award
settling frames. It cannot represent the requested experiment. Its optional
coordinate-consistency rejection is an unproven diagnostic, not mechanical
truth. The Crash-only ammunition key is obstacle advice and must not be part
of the autonomous policy.

Generic archive admission retains the cheaper equal-preference state in a full
slot. This rejects a strict same-key waiting continuation; ordinary random
suffixes contain at most six actions. A constant-key seven-step fixture is the
initial searcher counterexample. Unconditional replacement by later descendants
would discard the cheap representative and is not accepted without evaluating
cycle and destructive-drift behavior.

## First qualification

A fresh seed-1 whole-game run on msr1 completed 2,000 executions. Full campaign
replay reproduced its report and snapshots exactly; video replay matched its
decoded endpoint. It selected and entered a stage using searched inputs. No boss
or ending was reached. The report watermark includes raw stage-selection labels
and is not a trustworthy deepest-stage metric; inspect retained contexts and film.

Baseline uses the unchanged searcher from ea4d3116 and the new adapter. Two fresh
100,000-execution controls use seeds 1 and 2, five workers each, and 12,000 MiB
memory budgets. Their files live under `/root/mm2-autonomous-20260919/runs/`.
The adaptive-horizon branch is evaluated separately; equal seeds do not imply
equal actions after its policies diverge.

Qualification stream SHA-256:
`b547be50d2c046c1ec64b2c2a6066d7c92926f5a47f2484986a5c9c3d7527abf`.
Qualification checkpoint SHA-256:
`8b3b2dab1e685e66e55904db9c7ded5136c99314b5b820ab902c1e057cb6c9b3`.

## Observation audit after the first controls

Both fresh 100K controls reached multiple screens in all eight ordinary stages,
with no boss entry or kill. Seed 1 used 14,672,002 emulated frames and retained
49,371 active representatives; seed 2 used 14,746,809 frames and retained 46,795.
These counts alone are not progress evidence.

Seed 1 contained 18,206 retained endpoints whose bank-`0x0d` `$fd/$fe` values
were outside the source-defined weapon-menu cursor range. Across all bank-`0x0d`
entries there were 2,209 distinct raw pairs. The experimental raw tuple in v25
therefore split reused drawing/animation scratch into excessive local identities.
V26 removes that tuple from the key, bounds candidate menu selections by the
source's page/row ranges, and preserves raw bytes only as diagnostic observations.
Valid range alone is not a scene detector: Game Over reuses the same registers.

The generic horizon prototype passed its constant-action fixture but failed
real two-worker replay at job 2 (196 measured work units versus 97 recorded).
The live choice used reservation-time barren feedback, while replay initially
recomputed from admission-time feedback. That prototype is unqualified. The fix
records the chosen extension with the job and validates replay bounds; it needs
multiworker variable-action and real-game replay checks before long runs.

The preserved assisted tape passed the new target oracle: all 6,800 actions,
exact supplied-frame work, snapshot restoration, and ending at action 6697.
The six source-backed castle transitions were observed exactly once each.
The boot-only film independently confirms the fixed 17-action, 1,512-frame
prelude ends on the stage-selection grid. Boot tape SHA-256:
`d19eda0b7d33793a968db230a00746f5518892ffe4d1efa231b2113418cea96`.
None of these oracle inputs seed a qualifying campaign.

## Repaired replay qualification

The fixed core records its reservation-time horizon extension on job and skip
records (generic schema 6) and keeps extension policy private to the engine.
Default variable-length/variable-action multiworker tests now cover reservation
lag. A fresh 2,000-execution real-game run with the unchanged v25 adapter passed
full report/snapshot replay on msr1. Stream SHA-256:
`338e0dd06f412e6c48843420c51a2c757a0282bd1ea7374434e600c882de6ba3`.

The integrated v26 adapter (bounded menu identities and confirmed castle clears)
also passed a fresh 2,000-execution replay on the Mac, with stream SHA-256:
`153111dbb1a80a62ab6d09296420f78a11ef45388e320ce2c3a51a9b77aae108`.
MM2 stream/checkpoint format is v8. Its 45 active MM2 library tests and seven
CLI tests passed; the external-ROM oracle is separately opt-in. Targeted Clippy
passed. These are qualification checks, not game-completion claims.

At 23:02 UTC, the paired v25 seed-1 horizon arm and the integrated v26 seed-1
power-on campaign began fresh 100K budgets with five workers each. The longer
unchanged seed-1 control continues from its complete autonomous 100K archive;
archive import is a warm start, not an exact continuation of the random schedule.
