# Task 125 — revision-coordinator review-park cleanup batch

Land the five `revision-coordinator` follow-ups filed from the PR #124 tribunal, as **one
crate-scoped PR** (they all touch `dissonance/revision-coordinator/`, so batching avoids
intra-crate merge churn). Read each bead in full first — `bd show <id>` carries the exact
scope and the reviewer's rationale — and claim each as you take it.

Do these in an order that keeps the WAL/ledger correctness invariants intact; `hm-fb0` is
**timing-sensitive — land it before `hm-bbx.4` imports the crate**, so include it.

- **`hm-fb0`** (P2) — feature-gate the test apparatus (`StateProjection`, `MemFault`,
  `MemLedger::{crash,fail_next,durable_len}`) behind a `test-support` feature or narrow its
  visibility, BEFORE any external crate imports it and freezes it as compat surface
  (lib.rs ~36-41).
- **`hm-x4z`** (P2) — make `MemLedger` staging handle-local (mirror `FileLedger`'s `pending`)
  so a failed-sync record cannot be resurrected by a later sync. Sibling of hm-fb0.
- **`hm-a98`** (P2) — barrier watermark: track the first not-done cohort so `barrier_blocker`
  is O(1) instead of the current Θ(N²) full-cohort rescan (coordinator.rs ~217-223). Pure
  perf on a correctness-critical path — do not perturb the just-verified barrier semantics;
  add a test proving watermark == full-scan result.
- **`hm-20m`** (P2) — bound/document the abort-reason size so an oversized reason cannot
  poison-without-persisting; align `MemLedger`/`FileLedger` behavior (they diverged).
- **`hm-9xd`** (P3) — reconcile the Abort reason's "never state-affecting" annotation with
  its presence in `StateProjection::encode` (typed cause or a corrected doc annotation).

## Gates & done

Portable gates green scoped to the crate (fmt, clippy --all-targets -D warnings, nextest,
public-api snapshot — if the public surface changes, update `tests/public-api.txt` with the
diff justified in the PR). Keep every existing WAL/recovery/crash-recovery test green — this
crate just shipped, do not regress it. One PR; close each bead on merge. If any item turns out
to be larger than a park-level cleanup or reveals a correctness issue, split it out to its own
bead and say so rather than forcing it into the batch.
