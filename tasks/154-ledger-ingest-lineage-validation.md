# Task 154 — Ledger-ingest lineage validation: reject malformed lineage at the append/replay choke point (hm-wjv1)

**Bead:** `hm-wjv1` (P2, PR #156 B-park-1 + N-1 + N1 annotations — `bd show hm-wjv1`
and read all three comments first). **Surface:** `dissonance/explorer` — `ledger.rs`
(`EvidenceLedger::append` + the replay path), typed error addition, tests,
IMPLEMENTATION.md.

## Problem (three documented harm classes, one choke point)

`EvidenceLedger::append` validates content-address + budget only (ledger.rs:513-547).
It accepts:
1. **Self/cyclic parent references** (`RunId { issue: 7, parent: Some(7) }`, or mutual
   cycles) — `retention_report` was hardened to terminate (PR #156), but
   `compose_observations_at` itself has NO visited set and does not terminate on a
   cyclic `rollout.parent` chain (evidence.rs:429-462).
2. **Duplicate-issue Rollout batches** (content-distinct batches sharing one
   `rollout.issue`) — compose resolves first-match over ascending ids; collecting the
   first-sorting duplicate silently flips which batch every reader resolves to. Label
   stability across collection is unattainable while these are representable.
3. The general class: durable lineage shapes no producer emits and every reader must
   defend against individually. Rejection at ingest is the structural closure.

## Fix

Validate at the ONE choke point (`append`, and whatever the replay/open path uses if
distinct — cover both; a v4 ledger written by this build must never contain these
shapes, and replay of a hand-crafted file must refuse loudly):
- **Reject self-parent** (`issue == parent`) and **cycles**: an appended Rollout whose
  parent chain (through batches already in the ledger) revisits an issue is refused
  with a typed error. Appends are ordered, so a cycle can only close via the batch
  being appended — checking the new batch's chain is sufficient; state this invariant
  in a comment.
- **Reject duplicate-issue Rollouts**: a Rollout batch whose `rollout.issue` already
  has a Rollout batch in the ledger is refused (typed error naming both batch ids).
  Seals carry their OWN fresh issue and share the rollout's lineage via
  `parent: Some(rollout.issue)` (producer survey: campaign.rs:876-880) — the uniqueness
  constraint is per-role; do not break the rollout+seal pairing. [Corrected by the
  foreman post-PR #157 discovery: the original sentence "Seals share their rollout's
  issue by design" was wrong and misled two review seats.]
- **Parent-existence**: decide-and-document whether a `parent: Some(issue)` referencing
  an issue absent from the ledger is legal (out-of-order append / partial replay?).
  Survey in-tree producers and the replay order; if all producers append parents first,
  reject missing parents too; if not, document why dangling parents stay legal and
  fence them out of the cycle check. Do not guess silently — the survey goes in
  IMPLEMENTATION.md.

## Version question (answer explicitly, do not skip)

This RESTRICTS what a v4 ledger accepts but does not change the meaning of any
existing well-formed record. Per the ledger's own doctrine, a pure narrowing that
refuses previously-writable-but-never-produced shapes should NOT need a VERSION bump
(no honest v4 file contains them — verify that claim: no in-tree producer emits these
shapes; the PR #156 repros constructed them via the public API only). State the
reasoning in IMPLEMENTATION.md; if you find an honest producer of any rejected shape,
STOP and report instead of bumping VERSION on your own authority.

## Required regressions

1. Self-parent append refused (typed).
2. Mutual-cycle append refused at the closing append.
3. Duplicate-issue Rollout append refused (typed, both ids named); a Seal for an
   existing rollout still appends fine.
4. Replay of a serialized ledger containing each rejected shape refuses loudly
   (construct the bytes via the pre-fix writer pattern in the test, or a fixture).
5. The full existing suite stays green — if any existing test constructs a
   now-rejected shape, the test was exercising a defended-against artifact: migrate it
   to the typed-refusal assertion and say so in IMPLEMENTATION.md (the PR #156
   regression tests for cycle-termination and duplicate-issue labels will need exactly
   this treatment — keep the walk-hardening tests by feeding them via a test-only
   bypass ONLY if the hardening code stays reachable; otherwise re-scope them to the
   refusal and note that the walk bound is now defense-in-depth).

## Gates

Explorer + campaign-runner nextest, clippy `-D warnings`, fmt, `cargo mutants
--in-diff` 0 missed, hash-neutrality suites (ingest validation must not touch any
committed hash on honest runs — quote them). Public API: new error variants are
additive; regenerate the snapshot on the pinned nightly.

## Deliverable

PR from `task/ledger-ingest-lineage-validation` closing `hm-wjv1` with the merge.
