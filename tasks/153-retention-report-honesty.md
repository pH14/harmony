# Task 153 — retention_report recomputability honesty + observations_at translation qualifier (hm-f82p, hm-0qpm)

**Beads:** `hm-f82p` (P2, PR #147 discovery F3 residual / B1) and `hm-0qpm` (P2, PR #153
re-check C1). `bd show` both first. **Surface:** `dissonance/explorer` —
`campaign.rs` (`retention_report`), `evidence.rs` (one doc sentence + optional witness),
tests, IMPLEMENTATION.md. Two independent commits fine; one PR.

## 1. `hm-f82p` — retention_report overclaims recomputability

`campaign.rs:996-1007` (line numbers may have drifted — locate `retention_report`)
labels every retained batch `FromRetainedEvidence` even when an ancestor was collected —
overclaiming recomputability. Pre-dates PR #147 for child rollouts; now also covers
suffix-only seals. Fix: when any ledger ancestor of a retained batch has been collected,
the label must say so (a distinct variant or qualifier — follow the existing enum's
style; if the report type is public, regenerate `public-api.txt` on the pinned nightly
and flag it). Regression: a report over a graph with a collected ancestor must show the
honest label; a fully-retained graph keeps `FromRetainedEvidence`. This is a REPORT
labeling fix only — do not touch collection, compose, or fold behavior (the
compose-across-collected-ancestor behavior itself is `hm-4gaw`/`hm-btht` family, fenced).

## 2. `hm-0qpm` — observations_at up-to-translation qualifier

Reword `evidence.rs:362-367` (the coincidence sentence): for a `rollout.parent == None`
record, `observations_at(k) == compose_observations_at(led, ev, base + k)` where `base`
is the `parent_cut` count (0 when `None`); equality at matched raw arguments holds only
when the base is 0 (fixtures/legacy decodes). Optional witness (fixture already has
base 3): in `genesis_rollout_local_reduction_matches_composed_truth`, assert
`ev.observations_at(1) == compose_observations_at(&led, &ev, 4)` and
`!= compose_observations_at(&led, &ev, 1)`. Doc/test-only.

## Gates

Explorer nextest full; clippy `-D warnings`; fmt; `cargo mutants --in-diff` 0 missed;
hash-neutrality untouched (no hash-path code — state so). No dependency changes.

## Deliverable

PR from `task/retention-report-honesty` closing both beads with the merge. Minimal diffs.
