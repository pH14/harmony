# Task 144 — seal-past-rollout-terminal event truncation (T136-J5)

**Bead:** hm-aqf0 (P2 bug, PR #138 tribunal, CONFIRMED mechanism). Related: hm-esfd
(Option-C run-forward, merged as PR #138), hm-qcpp (maze M2, closed), hm-btht
(evidence-coverage family — separate, do not absorb).

## Problem (judge-confirmed, deterministic, hash-neutral surface)

Seal batches contribute no event rows (dissonance/explorer campaign.rs:1511-1514) and
reuse `rollout.normalized` (campaign.rs:721). When a candidate seal is advanced PAST the
rollout terminal (the PR #138 marker-clamped run-forward), the cut has
`cut.sdk_events > graph rows` — the cells/observation map deterministically OMITS the
advanced span's events.

## Fix direction (pick with evidence, record the choice)

Either (a) **capture the run-forward suffix into the seal batch** so the advanced span's
events reach the graph, or (b) **refuse/drop seals past the terminal**. Prefer (a) if the
suffix events are already normalized and reachable at the seal site (they ride the same
recorded env); choose (b) only if capture would violate a ledger/evidence invariant —
name it. Whichever way: fail-closed, no silent truncation remains.

## Acceptance

- A regression test pinning the current truncation shape (red before: an advanced-seal
  cut whose sdk_events exceed graph rows with the span absent; green after: either the
  span present (a) or the seal refused with a typed error (b)).
- Hash-neutrality: existing campaign/determinism suites bit-identical (the fix touches
  evidence/graph composition, not the RNG/schedule stream — verify with the existing
  bit-identical proptests).
- Full portable gates green (build + nextest + clippy + fmt + deny) for explorer +
  downstream campaign-runner.
- Evidence-recomputability discipline (the hm-efs contract direction): whatever lands
  must keep verdicts recomputable from committed artifacts.

## Scope

`dissonance/explorer/` (campaign evidence/seal path) + its tests. Do NOT take on
hm-btht's admission-time reseal/retry capture family or hm-kyy5's genesis-rooted
re-materialization — they are separate beads on the same neighborhood; keep this diff
truncation-scoped.

## Environment

Mac-local only (the truncation is portably reproducible per the tribunal record). No box.
Baseline model (Opus 4.8).
