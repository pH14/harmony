# Task 148 — Ledger VERSION 2→3 for the suffix-only Seal representation; refuse pre-144 ledgers loudly (hm-j7ie)

**Bead:** `hm-j7ie` (P2, PR #147 discovery F5 + verify V4 appended — read both comments
with `bd show hm-j7ie` first). **Surface:** `dissonance/explorer` — `ledger.rs` (VERSION
constant + open/refusal path), `campaign.rs`/`evidence.rs` only if a representation tag
turns out to be needed, tests, IMPLEMENTATION.md. **Unblocks on merge:** `hm-wshf` (the
accessor-contract bead is decided alongside — it stays its own task; do NOT fold it in).

## Problem (both halves already adjudicated)

PR #147 changed the meaning of a durable record without a version bump, violating the
ledger's own doctrine (ledger.rs:27; `VERSION = 2` at ledger.rs:65 with a loud
`UnsupportedVersion` refusal):

1. A **Seal record** now serializes the run-forward **suffix + observed cut** where it
   previously serialized the full rollout `normalized` + base-branch `parent_cut`
   (96042de7 campaign.rs:727-728 → current campaign.rs:752-753).
2. The **batch-identity preimage** (`canonical_bytes()`) therefore differs across the
   upgrade for the same seed (verify V4) — cross-version identity comparison is
   meaningless.
3. A pre-144 ledger's advanced seals **reopen with historically truncated cells,
   silently** — the exact silent-wrong class this project refuses everywhere else.

## Ruling encoded in this spec (foreman, doctrine-consistent default — Paul may veto at review)

**Bump `VERSION` 2→3 and REFUSE version-2 ledgers loudly** via the existing
`UnsupportedVersion` path (extend its message to name the reason: pre-144 seal records
are semantically truncated under the new walk). Rationale: the alternative — accepting
old ledgers — either silently reopens truncated cells (the F5 finding itself) or
requires a verified migration, and no migration demand exists today. Fail-closed is the
codebase's standing doctrine for exactly this situation. If a migration is ever wanted,
it is its own future task; this task must NOT build one. Record the accept-path
trade-off in IMPLEMENTATION.md so the review can weigh it.

## Requirements

- `VERSION = 3`; opening a version-2 (or any non-3) ledger fails loudly with a message
  naming the suffix-only representation change. No silent fallback, no read-old paths.
- Within-version determinism must be untouched: same-seed suites + the determinism
  proptest green, quoted in the PR.
- Regression tests: (a) a version-2 header is refused with the new message; (b) a
  freshly written ledger reopens cleanly at version 3 (round-trip); (c) the existing
  restart-rebuild suite stays green.
- Grep for any place that persists or compares `VERSION`/`canonical_bytes` across a
  reopen boundary and confirm none assumes version-2 shapes remain readable; list what
  you checked in IMPLEMENTATION.md.
- Scope fence: `hm-wshf` (accessors), `hm-mmkf` (fold routing), `hm-4gaw`, `hm-f82p`
  stay untouched. No wire-format changes outside the ledger header; no public-API
  changes unless the version constant is already public (if so, regenerate
  `public-api.txt` on the pinned nightly and say so).

## Gates

Full explorer + campaign-runner nextest, clippy `-D warnings`, fmt, hash-neutrality
suites. `cargo deny` only if dependencies change (they must not).

## Deliverable

PR from `task/ledger-version-3`: implementation + tests + IMPLEMENTATION.md subsection
stating the ruling, the refuse-vs-accept trade-off, and the checked reopen surfaces.
Close-with-merge: `hm-j7ie` (which unblocks `hm-wshf` in the tracker automatically).
