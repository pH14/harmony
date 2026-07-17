# IMPLEMENTATION — tasks/121, revision-coordinator (`hm-bbx.3`)

Mechanics, deviations, and integrator notes only; this file seeds the PR body
(the spec's write-up rule — no `docs/history` file).

---

## Task 125 — review-park cleanup batch (5 beads, one crate-scoped PR)

Five PR #124 tribunal follow-ups, all inside this crate. No production
behavior of the shipped coordinator changes; the WAL/recovery invariants are
untouched and every pre-existing test stays green (45 → 50 tests). Gates:
`fmt`, `clippy --all-features --all-targets -D warnings`, `nextest`
(all-features AND default), `cargo build` with no features under `-D warnings`
(the `hm-bbx.4` import view), `rustdoc` (both feature sets), `deny`, and the
regenerated `public-api.txt` snapshot — all green. No `unsafe`, so no Miri.

- **`hm-fb0` — feature-gate the test apparatus.** New `test-support` feature
  gates `StateProjection` + `Coordinator::state_projection`, `MemFault`, and
  `MemLedger::{crash, fail_next, durable_len}` (and `MemStore.fault` + its
  fault-check branches) behind `#[cfg(any(test, feature = "test-support"))]`.
  These are the crate's golden/recovery vehicle, not the coordinator's
  production contract; gating them keeps them OUT of the default public
  surface, so `hm-bbx.4` importing the crate without the feature never freezes
  them as compat surface. `MemLedger` itself (`new`/`Default`/`Ledger` impl)
  stays public — only its crash/fault/introspection extras are gated. The
  crate's own `tests/` self-enable the feature via a **dev-dependency on self**
  (the standard Cargo idiom), so a plain `cargo nextest` — no `--all-features`
  — still builds them; a plain library build stays clean. The public-api
  snapshot is taken with `--all-features`, so the frozen full surface is
  unchanged by the gating (a gated public item still can't drift unnoticed).
- **`hm-x4z` — `MemLedger` staging is now handle-local.** `MemLedger` grew a
  handle-local `pending: Vec<LedgerRecord>` mirroring `FileLedger::pending`;
  the shared `Rc` store now holds only durable (synced) records. `append`
  stages into `pending`; `sync` promotes `pending` into the shared log and
  clears it; `replay` returns the shared log (a handle's own un-synced staging
  is invisible, per the durability contract). A record whose `sync` failed on
  one handle therefore lives only in *that* handle's `pending` and can never be
  resurrected by another handle's later `sync` — the divergence from
  `FileLedger` the bead flagged. `reopen` (and `clone`) yield a fresh handle
  with EMPTY `pending`, so recovery handles start clean. New unit test:
  `failed_sync_record_is_not_resurrected_by_another_handle`.
- **`hm-a98` — O(1) barrier watermark.** `Core` grew a `done_through`
  watermark = the count of the contiguous prefix of cohorts that are all
  closed AND fully committed. Because the cohort barrier forces cohorts to
  complete in id order and a cohort never un-completes, the earliest not-done
  cohort is exactly `done_through + 1`, so `barrier_blocker` is O(1) instead of
  the previous full-cohort scan (Θ(N²) over a campaign). `done_through` is
  advanced (amortized O(1)) after every `commit`/`close`; the replay path uses
  the same mutators, so recovered and live state maintain it identically. Two
  tests prove watermark == full-scan for every `before`, after every mutation:
  a hand-built multi-cohort campaign and a 256-case proptest. The barrier
  *semantics* are unperturbed — the assign-side call stays invariant-dead
  defense-in-depth (the open cohort being assigned to is itself
  `first_incomplete`, so it returns `None`), matching the surviving mutant
  noted below.
- **`hm-20m` — bound the abort-reason size + align the two ledgers.** The
  abort `reason` is the only unbounded field of any record. `Coordinator::abort`
  now truncates it to `MAX_ABORT_REASON` (64 KiB, on a UTF-8 boundary) before
  recording — a post-mortem prefix is enough, and the bound (compile-time
  checked to survive worst-case ~6× JSON escaping under `MAX_FRAME_PAYLOAD`)
  guarantees the Abort frame always fits, so an over-long reason can no longer
  poison the coordinator *without* durably recording the abort. Separately,
  `MemLedger::append` now enforces the same frame bound as `FileLedger`
  (via the shared `MAX_FRAME_PAYLOAD`), so a hand-built oversized record is
  refused identically on both backends — they had diverged (the file ledger
  bounded records, the in-memory one did not). Tests:
  `oversized_abort_reason_is_bounded_and_still_persists` (both backends +
  restart) and `mem_ledger_refuses_oversized_record`.
- **`hm-9xd` — reconcile the Abort-reason annotation.** The reason's doc read
  "never state-affecting", yet the reason is serialized into
  `StateProjection::encode` and so is part of the byte-stable projection a
  recovered coordinator must match. Corrected the annotation (chosen over the
  bead's typed-cause alternative — see rejected deviations) to: the reason is
  *control-inert* (its text never changes the frontier, ordering, or any
  coordinator decision — only the presence of an abort does) but IS durably
  recorded, replayed verbatim, and therefore part of the state projection.

### Public-API snapshot diff (regenerated, justified)

Two lines, both from `hm-x4z`, no change to the coordinator's real contract:

- `MemLedger::crash(&self)` → `crash(&mut self)`: `crash` now discards this
  handle's local `pending` (which is where un-synced staging lives), so it
  needs `&mut`. Rippled to the three tests that crash a `MemLedger` handle.
- `impl Clone for MemLedger` now renders: I replaced `#[derive(Clone)]` with a
  manual impl that clones the store handle but starts `pending` EMPTY (a
  derived clone would copy staged records and could duplicate them on a later
  sync — the same class of bug `hm-x4z` closes). `MemLedger` was already
  `Clone`, so the bound is unchanged; `cargo public-api` simply renders manual
  impls where it omits derived ones.

### Deviations considered and rejected

- **`hm-9xd` typed abort cause (enum) instead of a doc fix.** Rejected as
  beyond a P3 park cleanup: it would change the public `LedgerRecord::Abort`
  and `CoordError::Aborted` shape, the golden, and many tests. The reason is
  genuinely control-inert; the accurate doc annotation resolves the
  contradiction the bead raised without a contract change.
- **`hm-fb0` narrowing visibility to `pub(crate)` instead of a feature.**
  Rejected: the apparatus is consumed by the crate's `tests/` (separate
  crates), which cannot see `pub(crate)` items. The `test-support` feature is
  the option the bead offers that keeps the integration tests compiling.
- **`hm-20m` bounding at the ledger only (not in `abort`).** Rejected:
  bounding solely at `MemLedger::append` would align the two backends onto the
  *wrong* behavior (both would then poison-without-persist on an over-long
  reason). The fix must bound at the source (`abort`) so the reason always
  fits; the ledger-level bound is the secondary alignment for hand-built
  records.

---

## What this is

The control-side input coordinator for the Differential observation plane:
persist-then-dispatch `Revision` assignment, out-of-order completion
buffering (`BTreeMap`), cohort freeze, probe-frontier drive over a live
one-worker Differential dataflow, and crash recovery by strict replay of an
append-only, fsync-ordered `Ledger`. All three milestones are green; every
binding public-API item from the spec exists with the specified semantics.

## Deviations that need reviewer eyes (all deliberate, all small)

- **Off-whitelist dependencies (ask-by-comment, rule 5):**
  `differential-dataflow` 0.24 and `timely` 0.30 — the task *is* the
  Differential input coordinator, and `docs/DISSONANCE-STRATEGY.md` rules
  "production differential-dataflow, one Timely worker". Same resolved
  versions as the ratified tasks/120 spike. `blake3` (whitelisted) for ledger
  frame checksums and digest-based ids.
- **Root `Cargo.toml`: one `exclude` line.** M2 requires a path dependency on
  the standalone `spikes/differential-lineage` workspace; Cargo refuses a
  nested workspace root inside the outer workspace's tree unless the outer
  excludes it (the same mechanism as `guest/`). No member globs changed.
- **`deny.toml`: two advisory ignores** (`RUSTSEC-2025-0141` bincode,
  `RUSTSEC-2024-0436` paste) — *unmaintained* notices, not vulnerabilities,
  pulled transitively by every current timely/DD release. The spike never hit
  them because its CI deny step is licenses-scoped; joining the root
  workspace graph exposed them to the full root gate. Documented in-place for
  re-evaluation at any timely/DD bump.
- **`Cargo.lock`** grew by the timely/DD/blake3 subtree (tracked file).

## Design decisions inside the spec's degrees of freedom

- **Construction is `genesis` (fresh ledger, pins a `CampaignConfigId`) or
  the binding `recover(&dyn Ledger)`.** A borrowed ledger cannot yield an
  owned writable handle, so the `Ledger` trait carries `reopen()` — an
  independent handle to the same durable log; recovery = reopen + strict
  replay. Replay validates the full write protocol (dense ids, view
  watermarks recomputed and compared, no double commit/close, nothing after
  an abort); any deviation is a typed `CorruptLedger`, never a panic.
- **Ids mint densely from 1** (`Revision`/`ProposalId`/`CohortId`);
  `Revision::ZERO` is the empty-frontier sentinel. Counters refuse to wrap
  (`IdExhausted`).
- **Two frontiers.** `committed_frontier` = contiguous committed prefix
  (what `drain_ready` submits to the dataflow); `visible_frontier` = the
  largest prefix in which every revision's cohort is closed AND fully
  committed. `probe_drive` reads only at the visible frontier, so no
  partial-cohort result can reach another proposal.
- **The full cohort barrier** (PR #124 FAM-COHORT ruling, option (a)):
  `open_cohort` and `assign` refuse (`CohortBarrier`) while any earlier
  cohort is not both closed and fully committed. Cohorts therefore run one
  at a time over contiguous revision ranges, visibility flips
  cohort-ATOMICALLY (the frontier only ever sits on a cohort boundary), and
  a frozen view is a constant of the schedule — never a function of
  completion arrival order, and never able to split a cohort's results
  across the frontier. Replay validation enforces the same barrier on the
  durable record stream. (Option (b) — cohort-atomic visibility with
  cross-cohort pipelining — is the documented future relaxation if ever
  needed; nothing in M2 required it.)
- **`probe_drive` stalls as a typed error** (`FrontierStalled`), not a
  block: the coordinator is single-threaded, so waiting would deadlock.
- **`Completion` carries the deterministic V-time/work `TerminalRecord`**
  (the strategy doc's "must end in a deterministic terminal record").
  A byte-identical retry is an idempotent no-op (crashed worker, same
  `ProposalId`); a divergent retry is `CommitConflict` — a determinism
  violation surfaced, never absorbed.
- **Ledger failure poisons the handle.** After a failed append/sync the
  coordinator refuses everything (`Poisoned`): an unrecoverable control
  failure aborts or recovers, never skips a slot. This also closes a real
  hazard: a half-staged record must never become durable behind a later
  successful sync from the same handle.
- **`drain_ready` returns empty after abort/poison** (binding signature has
  no `Result`); mutating verbs return `Aborted`. Clean `abort()` is durable,
  idempotent, and final — no later frontier advancement, verified through
  recovery.
- **The live dataflow is never authority.** `recover` rebuilds a fresh
  worker and re-feeds the committed prefix from the ledger on the next
  drain; the process-local submitted watermark is deliberately excluded from
  `StateProjection`, which is exactly the durable-derived state (that's what
  makes crashed-vs-never-crashed byte equality well-defined).
- **The worker runs without a wall clock**: timely's `Worker::new(..,
  now: None)` disables the logging registry and timer-based activations, so
  no nondeterministic clock exists anywhere in the graph (no clippy
  exceptions needed).

## Milestone/test map

- **M0** — `tests/coordinator_flow.rs` (buffering, freeze, retry, abort,
  recovery equality, mem/file byte-parity, golden encoded projection at
  `tests/goldens/projection.json`); `tests/permutation.rs` (256 cases:
  permutation-invariance + no-frontier-holes + cohort-freeze against a pure
  model, closure interleaved with completion arrival).
- **M1** — `src/file_ledger.rs` (append-only checksummed frames, staged
  appends, fsync barrier, torn-tail repair on open);
  `tests/crash_recovery.rs`: the `proptest-state-machine` model (256 cases)
  interleaves clean crashes and injected faults at both await points of the
  persist path (before append / between append and fsync), checks the
  projection against the reference model after every transition, and after
  every recovery compares projection AND probe artifacts byte-wise against a
  never-crashed twin replaying the same durable op log; plus a 48-case
  real-WAL variant (fsync-bound, hence fewer cases — the ≥256 crash gate is
  the model).
- **M2** — `tests/spike_integration.rs`: every spike-fixture revision
  becomes a digest-identified opaque batch; the effective fixture is rebuilt
  *from the coordinator's drained view* (resolve + restamp, including the
  replay vectors the referee slices), so a coordinator ordering/coverage/
  frontier bug corrupts the artifacts. Byte-identical across completion
  permutations, a mid-campaign crash+recover, and cohort-frozen mint order;
  both dataflow formulations equal the genesis-replay referee byte-wise
  (genesis replay == cached lineage plus suffix) on all three hand fixtures
  plus a dilated sparse-revision case where the restamp is non-identity.
- `tests/public_api.rs` + `tests/public-api.txt` — frozen-surface snapshot
  (pinned-nightly `cargo public-api`, repo pattern; `-- --ignored` in CI).

## Known limitations

- **WAL damage rules** (hardened per PR #124 FAM-WAL + the VERIFY event):
  the stream opens with `HWAL` + a u32 format version (unknown version =
  typed `UnsupportedVersion` refusal, F10). Frames are
  `len | len_check | payload_check | payload` — the length is
  independently verified (V1), so a tear may only be declared on a
  VERIFIED length: length prefix cut short → tear; verified length with
  incomplete payload → tear; everything else (length check failing with
  bytes present — the past-EOF corruption that used to masquerade as a
  tear and truncate committed records — over-bound length F5, payload
  check failing on a complete frame, undecodable payload) is a typed
  `Corrupt`, never silent truncation (F3). `FileLedger::open` fsyncs
  UNCONDITIONALLY before returning (F4 + V2: per-inode fsync also flushes
  a dead writer's page-cache-only frames, so the clean path has the same
  barrier as the repair path). The residual limitation: rot that exactly
  mimics a tear (physically truncating the file tail behind a verified
  length) is indistinguishable from one by construction.
- **`sync` durability is `File::sync_data`** (fsync). On macOS that does not
  issue `F_FULLFSYNC`; good enough for the portable gates and the crash
  model, and the production backing is `hm-bbx.4`'s anyway.
- **Single-threaded by design** (one Timely worker per the spec; `MemLedger`
  uses `Rc`, the worker is thread-local). Parallel dispatch happens in the
  caller; the coordinator is the serialization point, which is the doctrine.
- **The WAL never compacts** — append-only for the campaign's lifetime,
  matching the sealed-campaign model (retention/GC is explicitly out of
  scope here).
- `reopen()` while the original handle still writes is split-brain and
  unsupported (documented on the trait; recovery-only).

## Scoped mutants (PR #124 batch requirement)

`cargo mutants` over `ledger.rs` + `file_ledger.rs` + `coordinator.rs`:
152 mutants — 123 caught (after adding `tests/replay_validation.rs` and the
exact frame-bound test), 18 unviable, 6 timeouts, 5 accepted survivors,
each argued:

- `coordinator.rs barrier_blocker` `< → >` on the `before` bound: the
  assign-side barrier is invariant-dead defense-in-depth — a cohort can
  only open when every earlier cohort is done and done never regresses, so
  no legal API sequence or replayable stream reaches it (the ruling asked
  for the refusal on both verbs; it stays).
- `coordinator.rs assign` `|| → &&` on the id-exhaustion guard:
  `next_proposal` and `next_revision` advance in lockstep and are always
  equal, so the operators are indistinguishable (belt-and-braces check).
- `coordinator.rs drain_ready` `< → <=`: the extra iteration hits the
  `committed.get(...) else break` guard immediately — equivalent mutant.
- `file_ledger.rs open` `delete !` on `created`: flips WHEN the parent
  dirsync happens (skip-on-create/extra-on-reopen); dirsync effects are
  not observable without filesystem-level crash injection.
- `file_ledger.rs open` `< → <=` on the repair condition: fires a
  truncation to the file's current length — a no-op plus a redundant
  fsync; behaviorally invisible.

## Integrator notes

- `hm-bbx.4` supplies the production `Ledger` implementation over the
  evidence ledger and resolves `EvidenceBatchId`s to payloads; this crate
  treats both as opaque. `probe_drive`'s `DrainedView` is the only
  sanctioned read path for search-visible inputs.
- The root `exclude` line must survive future root-manifest edits or the
  M2 dev-dependency stops resolving.
- No `unsafe` anywhere; Miri not required (spec). The crate joins the
  workspace by the existing `dissonance/*` glob; no CI edits.
