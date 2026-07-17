# Task 121 — Deterministic Differential revision coordinator (`hm-bbx.3`)
Control-side input coordinator for the Differential observation plane (epic `hm-bbx`, ruled GO by
`docs/DISSONANCE-STRATEGY.md`; doctrine: branch-is-a-key, `Revision` is the ONLY timestamp, no
custom lattices, one Timely worker initially; the merged `spikes/differential-lineage` crate
defines the proven dataflow shapes). Claim `hm-bbx.3` first (`bd update hm-bbx.3 --claim`).

## Goal

Build the **control-side input coordinator** that turns the imperative, seeded search loop's
completion stream into a frontier-closed, crash-recoverable Differential input log: persist each
proposal's `Revision` assignment *before* dispatch, atomically commit each already-durable
evidence-batch identity to its `Revision`, order revisions by seeded issue order, and buffer
out-of-order completions until the frontier may advance. The coordinator drives Differential
probes and rebuilds from the durable ledger on restart with no frontier holes.

## Non-goals

- **Decode SDK payloads or materialize VMs** — `hm-bbx.4` (evidence-ledger payload
  append/replay + `TraceStore` bridge); this child treats every batch identity as an opaque,
  already-durable token.
- **Define new Differential relations, arrangements, or cell reductions** — fixed by the
  merged spike crate; this child submits inputs to them, it does not author them.
- **Choose or mutate campaign configuration** — the coordinator is a deterministic
  ordering/commit machine under a fixed `CampaignConfig`; Resolution and the Explorer own
  adaptation.

## Deliverable

A new crate `dissonance/revision-coordinator/` with the following public API (names and
semantics are binding):

```rust
/// Monotonic Differential logical timestamp. The ONLY timestamp; never wall-clock.
pub struct Revision(u64);
/// Identity of a proposal persisted before dispatch (seeded issue order).
pub struct ProposalId(u64);
/// A frozen cohort: fixed selector/archive view, canonical proposal mint order.
pub struct CohortId(u64);
/// Opaque, already-durable evidence-batch identity supplied by hm-bbx.4.
pub struct EvidenceBatchId(/* digest-based, opaque */);
pub struct PendingProposal { proposal: ProposalId, revision: Revision, cohort: CohortId }
pub struct Completion { proposal: ProposalId, batch: EvidenceBatchId /* + terminal record */ }
pub struct Coordinator { /* proposal ledger, completion buffer, probe frontier state */ }
impl Coordinator {
    /// Persist proposal→Revision assignment and the cohort view BEFORE dispatch; never reuses a Revision.
    pub fn assign(&mut self, cohort: CohortId) -> Result<PendingProposal, CoordError>;
    /// Atomically commit an already-durable batch identity to its proposal's Revision; buffers
    /// out-of-order completions and never advances the frontier past a gap.
    pub fn complete(&mut self, c: Completion) -> Result<(), CoordError>;
    /// Drain contiguous Revision-ordered completions up to the first unmet slot, advancing the probe frontier.
    pub fn drain_ready(&mut self) -> Vec<(Revision, EvidenceBatchId)>;
    /// Drive Differential probes until the search-visible frontier passes `target`, then return
    /// consolidated, canonically ordered inputs. No partial-cohort result reaches another proposal.
    pub fn probe_drive(&mut self, target: Revision) -> Result<DrainedView, CoordError>;
    /// Replay the durable ledger; recover frontier and pending proposals exactly.
    pub fn recover(ledger: &dyn Ledger) -> Result<Self, CoordError>;
}
```

The **persist-then-dispatch handshake**: `assign` writes the proposal record (cohort, seeded
ordinal, reserved `Revision`) to the append-only ledger and flushes *before* returning a
`PendingProposal` the caller may dispatch. A worker crash after `assign` retries the *same*
`ProposalId`; an unrecoverable host/control failure aborts the campaign, never skips a slot.
The **completion-buffer** is a `BTreeMap` keyed by `Revision`; `drain_ready` emits contiguous
prefixes and stops at the first hole. The **probe-drive loop** blocks search-visible reads until
the probe frontier has passed the submitted `Revision`: every relation is read only after its
frontier clears, then consolidated and canonically ordered before it can affect selection or
serialized bytes.

Define an append-only, fsync-ordered `Ledger` trait here; `hm-bbx.4` supplies the concrete
evidence-payload backing. Over a frozen `CohortId`, `assign` mints proposals in canonical order
and the cohort's selector/archive view does not move until its frontier closes; no
partial-cohort result is readable by another proposal.

## Context you must read first

- `docs/DISSONANCE-STRATEGY.md` — §"The sealed campaign is the adaptation boundary" and the
  Revision-coordinator paragraphs (persist-before-dispatch, completion-order buffering, cohort
  freeze, crash recovery, probe frontier).
- The merged `spikes/differential-lineage` crate for the dataflow shapes and probe semantics
  this coordinator must respect; the epic bead (`bd show hm-bbx`) and `hm-bbx.1`/`hm-bbx.4`
  boundaries; `tasks/00-CONVENTIONS.md`.

## Milestones

### M0 — In-memory coordinator + property tests

`assign`/`complete`/`drain_ready`/`probe_drive` over an in-memory `Ledger` (no fsync), one Timely
worker, `Revision` from seeded issue order. Property tests (proptest ≥256):
**permutation-invariance** — any completion arrival order yields the identical `drain_ready`
sequence and consolidated artifacts; **no-frontier-holes** — `drain_ready` never emits a
Revision with an unmet predecessor; **cohort-freeze** — no partial-cohort result is observable
to a later proposal.

### M1 — Durable assignment log + crash-recovery tests

Append-only file-backed `Ledger` (`memmap2`/`tempfile`, fsync before `PendingProposal` returns;
portable, no Linux-only syscalls). Crash-recovery tests: a `proptest-state-machine` model
kills the coordinator between each `assign`/`complete`/flush, then `recover`; assert frontier
and pending set are byte-identical to a never-crashed run of the same seed and completion set.
Worker crash ⇒ retry SAME `ProposalId`; unrecoverable failure ⇒ abort, never skip a slot.

### M2 — Integration against the spike dataflow

Wire `probe_drive` to the merged spike crate's dataflow: submit committed
`(Revision, EvidenceBatchId)` inputs in seeded order, drive probes, and assert **identical
artifacts** across (a) input permutation, (b) a restart, and (c) cohort-frozen mint order.
Genesis replay equals cached lineage plus suffix — byte-wise.

## Gates

```sh
cargo build -p revision-coordinator --all-features
cargo nextest run -p revision-coordinator --all-features
cargo clippy -p revision-coordinator --all-features --all-targets -- -D warnings
cargo fmt -p revision-coordinator -- --check
cargo deny check
```

Plus task-specific: proptest permutation and crash suites (≥256 cases each);
`proptest-state-machine` for the kill-at-every-await-point recovery model; a `cargo public-api`
snapshot. **Determinism gate**: identical artifacts (byte-wise) across input permutation and
restart, asserted against a golden encoded projection — a divergence is blocking, not a nit. No
`unsafe` is introduced; Miri is not required.

## Determinism implications

- **Revision is the only timestamp.** Every state-affecting order comes from the seeded issue
  order, never completion order, wall-clock, thread scheduling, or worker-arrival order. One
  Timely worker alone does not make input ordering deterministic; the seeded `assign` order does.
  No floats in state-affecting code; all ids are `u64` with a total, seed-derived order.
- **No hash iteration reaches output** — the completion buffer is a `BTreeMap`; drained views are
  consolidated and canonically sorted before they can affect selection or serialized bytes
  (clippy `disallowed_types`/`disallowed_methods` lints hold).
- **Persist-before-dispatch** closes the frontier-hole and crash-skip vectors: a slot without
  a durable proposal cannot be claimed by completion order, and a retried worker reuses the
  same `ProposalId`/`Revision`. An unrecoverable failure aborts rather than skip.
- **Restart is deterministic**: `recover` replays committed ledger inputs; the live arrangement
  is never authority; genesis replay equals cached lineage plus suffix.

## Environment

Pure Mac-portable logic. No box, no `/dev/kvm`, no wall-clock source, no Nimbus machine.
File-backed ledger uses `tempfile` + `memmap2` only; builds and all gates pass on macOS and Linux.

## Definition of done

All milestones green, gates pass, public API matches this spec, write-up in the PR body per
the hygiene ruling (no docs/history file). `hm-bbx.3` closes on merge.
