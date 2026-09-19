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
