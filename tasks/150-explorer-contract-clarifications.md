# Task 150 — Explorer contract clarifications: version-refusal message + Seal-record accessor contract (hm-s6cb, hm-wshf)

**Beads:** `hm-s6cb` (P2, PR #151 F1) and `hm-wshf` (P2, PR #147 V5 — unblocked by the
merged hm-j7ie VERSION-3 ruling). `bd show` both first. **Surface:**
`dissonance/explorer` — `ledger.rs` (message only), `evidence.rs` (accessor docs/shape),
tests, IMPLEMENTATION.md. Two independent commits are fine; one PR.

## 1. `hm-s6cb` — version-refusal message must not misdiagnose future versions

`ledger.rs:325-327` routes every `found != VERSION` through one static string that
describes the file as a pre-144 ledger whose "advanced seals would reopen with
historically truncated cells" — false for a `found > 3` file from a future build. Keep
the refusal exactly as loud and early; fix only the rationale: version-neutral phrasing,
or a `found < 3` conditional tail (keep the suffix-representation sentence for the
`found < 3` case; a plain "newer than this build understands" for `found > VERSION`).
Add a `found: 4` regression alongside the existing `found: 1`/`found: 2` ones.

## 2. `hm-wshf` — accessor contract on suffix-only Seal records

`observations_at_cut`/`observations_at` reduce `self.normalized.events` only
(evidence.rs:304-317) against a doc saying "the reduced observation map true at this
evidence's own cut". On a post-144 Seal record that is the **suffix alone** (empty map
for a non-advanced seal of a state-bearing rollout). With the VERSION-3 ruling merged,
suffix-only is the ONLY readable Seal representation, so this is live contract drift.

**Direction (settled alongside the hm-j7ie ruling — do not redesign):** a single
Evidence record cannot compose (composition needs ancestor access; that is
`compose_observations_at`'s job, and retention's Seal arm + the parity oracle already
use it). So the closure is **re-document + fence, not compose-aware accessors**:
- Rewrite the accessor docs to state exactly what they return: the record-LOCAL
  reduction (for post-144 Seal records, the advanced-span suffix alone), and point
  callers needing the true cut view at `compose_observations_at`.
- Add a debug-assertion or doc-test-shaped example making the Seal-record behavior
  explicit (a Seal record's local map vs the composed map differ — show it).
- Audit the (test-only) callers the V5 record names (campaign.rs no-panic restart
  check; retention.rs directly-constructed-record test) — confirm each either wants the
  local reduction or is migrated to `compose_observations_at`; say which in
  IMPLEMENTATION.md.
- If you conclude renaming the accessors is warranted (e.g. `local_observations_at`),
  that is a public-API change: regenerate `public-api.txt` on the pinned nightly and
  flag it in the PR. Renaming is acceptable but not required; misleading docs are the
  defect.

## Gates

Explorer nextest full; clippy `-D warnings`; fmt; hash-neutrality suites only if any
non-doc code changes on the evidence path (a rename or debug assert does not touch
hashes — state so). No dependency changes.

## Deliverable

PR from `task/explorer-contract-clarifications` closing both beads with the merge.
Minimal diffs; the refusal stays loud, the accessors stay honest.
