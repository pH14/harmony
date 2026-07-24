# Task 155 — v1-verb-capable test machine: end-to-end advanced-span verdict gate + StepReport seal-counterexample surface (hm-5mx0)

**Bead:** `hm-5mx0` (P2, PR #155 F2+F3c merged — `bd show hm-5mx0` and read the
PR #155 adjudication's F2/F3c sections first). **Surface:** `dissonance/explorer` —
test machinery (testkit or a sibling test module), `campaign.rs` (StepReport surface),
tests, IMPLEMENTATION.md. One infrastructure unlock closes both halves.

## Problem

1. **F3c**: no end-to-end gate drives an advanced-span occurrence/assertion hit through
   the production `capture_seal_suffix` → `decode_child_suffix` →
   `DifferentialCampaign::step` path. Blocked because `ScriptedMachine` emits a **v2
   catalog** (no verb → `AssertType::None` → neither verdict fold fires), state events
   only, Quiescent seals. The PR #155 red-before test had to work at the fold surface
   instead — an adjudicated, documented deviation whose residual this task closes.
2. **F2**: the seal fold's `new_counterexamples` are dropped from `StepReport`
   (campaign.rs:677 initializes from the rollout fold only; :786-796 reads only
   `fold2.admitted`); the fingerprint is already in `seen_counterexamples`, so a
   seal-only counterexample's details (property, kind, Moment) can never surface in any
   later step — recoverable from raw evidence only until GC. Zero consumers today, so
   this is P2 — but the field's contract should become true, not stay documented-false.

## Deliverables

1. **A v1-verb-capable test machine**: emits a v1 catalog declaring occurrence/assertion
   properties WITH verbs (so `AssertType` resolves and the folds fire), can emit
   assertion/occurrence firings at chosen Moments — including inside an advanced span
   `[rollout_terminal, seal_cut)` — and supports the marker-clamped run-forward seal
   shape (tasks/144 lineage). Build it as test machinery (testkit-style), reusing the
   existing v1 catalog/firings encoders (PR #155 F5 noted three hand-written copies —
   consolidate into the one you build on, closing that note). Model it on
   `ScriptedMachine`'s Machine-trait surface; do NOT modify `ScriptedMachine` itself
   (its v2 shape is load-bearing for existing suites).
2. **The true end-to-end advanced-span verdict gate**: a campaign step in which an
   occurrence (sometimes) hit exists ONLY in the advanced span, driven through the
   production capture/decode/step path, asserting the verdict judges it present
   (red-before: disable the Seal-arm fold as PR #155's test did, and quote the failure).
   Include the absence direction (a must-hit declared and fired only in the advanced
   span is satisfied, not falsely absent).
3. **StepReport surface**: merge the seal fold's `new_counterexamples` into
   `StepReport.counterexamples` (same dedup semantics as the rollout fold — a
   fingerprint already in `seen_counterexamples` stays deduped; a seal-only NEW
   counterexample surfaces with its details). Update the field doc (remove the
   rollout-fold-only caveat added by PR #155). Test: a seal-only counterexample appears
   in the step's report exactly once; a rollout counterexample re-fired in the seal
   span does not duplicate.

## Constraints

- Hash-neutrality: `StepReport` is a report, but verify nothing feeds committed hashes;
  run the same-seed suites + determinism proptest and quote them green.
- Scope fence: `hm-w1o6`, `hm-4gaw`, `hm-6x0w`, `hm-avvc`, `hm-g2bq` untouched. No
  ledger changes; no wire-format changes. Public-API: `StepReport` fields are public —
  if the shape changes (it should not; you are filling an existing field), regenerate
  the snapshot and flag it.
- If the v1-verb machine turns out to require production-code changes to be drivable
  (beyond test machinery), STOP and report the exact gap instead of changing the
  contract on your own authority.

## Gates

Full explorer + campaign-runner nextest, clippy `-D warnings`, fmt, hash-neutrality
suites + determinism proptest, `cargo mutants --in-diff` 0 missed. No dependency
changes.

## Deliverable

PR from `task/v1-verb-test-machine` closing `hm-5mx0` with the merge.
