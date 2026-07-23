# Task 152 — Route advanced-span occurrence/assertion events through fold_batch's verdict folds (hm-mmkf)

**Bead:** `hm-mmkf` (P2, PR #147 discovery F4 — `bd show hm-mmkf` and read the PR #147
adjudication's F4 section first). **Surface:** `dissonance/explorer` —
`retention.rs` (`fold_batch`), possibly `campaign.rs` call sites, tests,
IMPLEMENTATION.md. Scope-fenced: `hm-btht` (capture-side evidence-coverage family),
`hm-4gaw`, `hm-f82p`, `hm-w1o6` untouched.

## Problem

`fold_batch` invokes `OccurrenceOracle` and the absence fold only in the **Rollout**
arm (campaign.rs:1566 hand-off, retention.rs:409-417). Since PR #147 the advanced span
`[rollout_terminal, seal_cut)` is durably captured in the **Seal** batch — but its
occurrence/assertion events are never judged. A sometimes-hit or assertion event that
exists ONLY in the advanced span yields a **false absence** in campaign verdicts. The
capture side is done (PR #147); this fix is **pure fold-side** (the F4 record's own
framing).

## Fix

Run the occurrence/assertion verdict folds over the Seal arm's suffix events as well:
- The Seal arm in `fold_batch` must feed the batch's `normalized.events` (the suffix)
  through the same `OccurrenceOracle`/absence machinery the Rollout arm uses — same
  oracle instance/keying so a hit in the advanced span counts toward the same
  occurrence identity as a rollout hit (no new keying scheme; reuse the Rollout arm's).
- Idempotence/duplication guard: a NON-advanced seal (suffix empty) must contribute
  nothing; an advanced seal's suffix must contribute exactly its own span — prove no
  event is double-judged (the rollout's own events are not in the seal suffix
  post-PR #150, but write the test that proves it rather than asserting it).
- If the folds' verdict state feeds any committed hash, hash-neutrality is REQUIRED on
  workloads with no advanced-span occurrence events, and the changed verdicts on
  advanced-span workloads are the intended fix (document the before/after in
  IMPLEMENTATION.md). Run the same-seed suites + determinism proptest and quote them.

## Required regression tests

1. **False-absence closure (red-before)**: a sometimes/occurrence event emitted ONLY in
   the advanced span (between the rollout terminal and the seal cut) is currently
   reported absent; after the fix it is judged present. Build on the tasks/144 repro
   shape (marker-clamped run-forward past the terminal). Quote the red-before run.
2. **Non-advanced seal contributes nothing**: verdicts identical with and without the
   Seal-arm fold for a workload whose seals are all at-terminal.
3. **No double-judging**: an occurrence event in the rollout body is counted once, not
   re-counted via any seal batch.

## Gates

Full explorer + campaign-runner nextest, clippy `-D warnings`, fmt, hash-neutrality
suites + determinism proptest, `cargo mutants --in-diff` 0 missed. No dependency
changes; no wire-format changes; public-api only if a fold signature is public (state
so and regenerate the snapshot on the pinned nightly if touched).

## Deliverable

PR from `task/advanced-span-verdict-folds` closing `hm-mmkf` with the merge.
