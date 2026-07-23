# Task 146 — Complete the seal-capture reconciliation: count invariant + prefix commitment (hm-whoo)

**Bead:** `hm-whoo` (P2, carries the PR #147 V3 finding plus the re-check C1 appendix —
read both comments on the bead with `bd show hm-whoo` before writing code).
**Surface:** `dissonance/explorer` — `campaign.rs` (`capture_seal_suffix` and the
`SealSuffixDivergence` path), `error.rs` docs, tests. No wire-format, ledger, or
public-API semantic changes beyond what is listed here.

## Problem

PR #147 landed a one-frame seal-capture reconciliation, but it constrains only the
**decoded suffix length**, leaving two verified holes (both divergent-host-triggered,
judge-CONFIRMED mechanisms, graded P2 by the standing trigger-profile rule):

1. **C1 — below-baseline bypass** (re-check, judge-reproduced at `7f7bbda4`): within
   `[0, baseline]` the stamp is completely unconstrained against the capture —
   `expected = cut.sdk_events.saturating_sub(rollout_raw_len)` is 0 and any capture
   `≤ baseline` decodes to an empty suffix, so **any** (stamp ≤ baseline, capture ≤
   baseline) pair passes, including every interior seal. An under-stamp excludes a
   captured firing from the committed cell; an over-stamp (still ≤ baseline) includes
   inherited rows the sealed state never reached. Both are silent wrong evidence.
2. **V3 — same-length prefix divergence** (verify event): only the count is checked; an
   equal-length prefix-divergent capture composes a hybrid state.

## Fix (one choke point, judge-checked direction)

- **Count half:** the complete honest count invariant is `cut.sdk_events == raw.len()` —
  compare the **raw capture length** directly against the stamp **before decoding**.
  This subsumes the current suffix-length check and closes the below-baseline hole.
  Keep the typed refusal (`SealSuffixDivergence` or a renamed variant if the fields no
  longer fit — if the error's public shape changes, regenerate `public-api.txt` on the
  pinned nightly and say so in the PR).
- **Content half:** anchor a **prefix commitment** so an equal-length divergent prefix is
  refused. The rollout's `Normalized.commitment` is the existing anchor the verify
  disposition names — reuse it; do not invent a new hash surface. If reusing it turns out
  to be structurally impossible from the seal path, STOP and report why in the PR rather
  than inventing an alternative commitment scheme.
- **Docs:** fix the overclaim at `capture_seal_suffix` / `error.rs` ("a short or
  count-divergent host capture … is refused loudly" — currently untrue at or below the
  baseline; after the count half it becomes true, and the comment must also state what
  the prefix commitment does and does not cover).

## Required regression tests (all four; the first three have judge-verified shapes)

1. Below-baseline **under-stamp** (host stamps 1 against its own 2-record capture,
   baseline 2) → refused loudly. This is the C1 repro; today it is ADMITTED.
2. Below-baseline **over-stamp** (stamp ≤ baseline but > capture) → refused loudly.
3. **Honest host still admitted**: the existing honest-production-frame test
   (stamp = `inner.sdk_events().len()`, `step()` succeeds) must stay green — the C1 fix
   must not reintroduce the V1 false refusal. Run it by name and quote it green.
4. **Prefix-divergent same-length capture** → refused via the commitment half.

## Constraints

- Determinism/hash-neutrality: the fix must not change any committed hash on honest
  runs — run the same-seed suites + the determinism proptest and quote them green.
- Scope fence: `hm-j7ie` (ledger VERSION/representation), `hm-wshf` (accessor contract,
  blocked on hm-j7ie), `hm-mmkf`, `hm-4gaw`, `hm-f82p` are parked — do NOT touch their
  surfaces. The retention/fold arms and `evidence.rs` walk are out of scope.
- Gates: full explorer + campaign-runner nextest, clippy `-D warnings`, fmt, and the
  hash-neutrality suites; `cargo deny` only if dependencies change (they must not).

## Deliverable

PR from `task/seal-capture-reconciliation`: implementation + the four tests + doc fix +
an IMPLEMENTATION.md subsection (per conventions) recording which half closes which hole
and quoting the honest-host test green. Close-with-merge: `hm-whoo`.
