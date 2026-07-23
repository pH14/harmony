# Task 142 — MockBackend late-landing variant: make the arm-seam failure shape portably real

**Bead:** hm-40na (P2, PR #143 F1 residue, judge-filed). Relates: hm-x1ss (this is
acceptance material for the schedule-closure decision), hm-zwhi (the bug this makes
portably reproducible).

## Problem

`MockBackend::run_until` (consonance/vmm-core mock, ~mock.rs:393-399) unconditionally
rewrites `reached := deadline`, so the mock can never land LATE — it always stops exactly
where asked. The real @3e7 failure shape on the box (a staged Moment's arrival arm declined
or clobbered, the guest free-running to the next natural boundary, overshoot < 1 quantum)
is therefore unreproducible portably; PR #143's regression had to use a ratio-1000 clock
fixture as a proxy instead of the genuine late-landing mechanism.

## Work

Add a **late-landing capability** to MockBackend: a scripted/configurable variant where a
`run_until(deadline)` leg lands at a scripted point PAST the requested deadline (the
next-natural-boundary shape), while default behavior stays byte-identical for every
existing test. Then:

1. A regression test that drives the MERGED arm-seam guard (`ScheduleMomentUnreachable`,
   landed in PR #143) from a genuinely late landing — the refusal fires because the
   backend actually cannot clamp, not because a fixture ratio made the Moment off-grid.
2. A test pinning the poison-latch semantics under the late-landing path (same contract
   the PR #143 F4 assertions pin: post-refusal perturb/snapshot rejection, replay clears).
3. Determinism discipline: the scripted lateness is an explicit test input (no randomness,
   no time); all existing vmm-core/campaign-runner/explorer suites pass unchanged.

## Scope guards

- Mock + tests ONLY — no production control-path or backend-trait changes. If the variant
  cannot be expressed without touching a production seam, STOP and report the exact
  constraint (that finding is hm-x1ss input, not yours to fix).
- Do NOT design the @3e7 cure (bounded-late drain etc.) — hm-x1ss owns it.
- Do NOT re-litigate PR #143's guard semantics; you are making its failure shape testable.

## Acceptance

- Portable gates green (build + nextest + clippy + fmt + deny) across vmm-core and the
  downstream campaign-runner/explorer suites (hash-neutrality: all existing tests
  unchanged).
- The two new tests above, plus: with the arm-seam guard hypothetically disabled (comment
  in the test naming the mutation), the late-landing variant would drive the OLD silent
  overshoot — i.e., this is the red PR #143's proxy fixture could not express. Express
  that as a mutation-style assertion or a documented manual-mutation note, matching how
  control.rs's existing tests pin their guards.
- If the new mock paths involve `unsafe` (they should not): Miri-reachable per the
  unsafe⇒Miri rule.

## Environment

Mac-local only. No box, no Nimbus. Baseline model (Opus 4.8).
