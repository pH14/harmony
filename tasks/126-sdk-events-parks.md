# Task 126 — sdk-events park + correctness cleanup batch

Land the three `sdk-events` follow-ups as **one crate-scoped PR** (all touch
`dissonance/sdk-events/`). Read each bead in full first (`bd show <id>`) and claim each.

- **`hm-jyj`** (P2) — a malformed setup status currently fabricates `setup_complete`. Make
  malformed setup input fail totally (reject) rather than silently synthesizing a completed
  setup. This is a correctness fix, not cosmetic — add a hostile-input test that pins the
  rejection.
- **`hm-b2g`** (P2) — the ingress-strictness parks batched from the PR #120 r12 review. Work
  the specific items the bead enumerates; each is a strictness tightening on ingress decode.
- **`hm-ynt`** (P2, bug) — SDK event Moments are being treated as emission Moments, but they
  are V-time-**anchor lower bounds** (~27-frame skew observed). Correct the semantics so a
  Moment is interpreted as the anchor lower bound, not the emission instant, and document the
  contract. This interacts with the seal-cut work (tasks/127 / hm-bbx.6) — the included-count
  cut is by SDK-vector prefix, NOT by Moment, so this fix must not reintroduce Moment-as-cut
  reasoning. Coordinate the contract wording with the GLOSSARY if it touches shared vocab.

## Gates & done

Portable gates green scoped to the crate (fmt, clippy --all-targets -D warnings, nextest,
public-api snapshot — justify any surface change in the PR). Keep the just-merged
re-decode-and-compare / StreamCommitment integrity tests green (hm-bbx.1 shipped this crate —
do not regress the artifact-integrity guarantees). One PR; close each bead on merge. Escalate
rather than guess if `hm-ynt`'s semantics correction turns out to change a wire contract other
crates depend on.
