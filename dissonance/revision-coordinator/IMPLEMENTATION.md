# IMPLEMENTATION — tasks/121, revision-coordinator (`hm-bbx.3`)

Mechanics, deviations, and integrator notes only; this file seeds the PR body
(the spec's write-up rule — no `docs/history` file).

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
