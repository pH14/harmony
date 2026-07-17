# tasks/130 (hm-bbx.4) — Explorer ↔ Differential cells + archive integration

The culmination of the Differential-migration epic (`hm-bbx`): the generic Explorer's
production search loop in its Differential-integrated form, plus the `sdk-events`
cycle-break/legacy-deletion and the `campaign-runner` re-sourcing that its acceptance
criteria require. This is the review-grounding record (the multi-crate write-up the
conventions place in the PR description; kept here durably because a later task cites it).

## Scope decision (Paul, 2026-07-17)

Ruled **Option 1** — the faithful, bounded, laptop-verifiable increment:

- **Delete** only the reusable-sensor path from `sdk-events` (`LinkSensor`, the LINK
  channels, packed `(register,value) → FeatureId`, `AlwaysViolation`, and the legacy
  `GuestEvent` decoder/catalog) — satisfying the "DELETE from production / legacy path
  unreachable" invariant.
- **Build** in the Explorer, ADDITIVELY: the crash-safe evidence ledger, the two-barrier
  replay controller, and the occurrence Oracle. The shared task-64 spine
  (`Sensor`/`Feature`/`FeatureSet`/`CellFn`/`Archive`/`CoverageArchive`/`GuestEvent`/
  `COVERAGE_CHANNEL`) is **kept intact** — `matcher`, `logtmpl`, and `runtrace` consume
  it in production source, so deleting it would explode the blast radius to 6+ crates,
  far past this bounded increment.
- **Relocate** the game-specific packed-`(reg,value)` cell derivation into
  `campaign-runner` over the normalized `SdkEvent` reduction (game policy, not a reusable
  sensor).
- The full DD vertical (production relations inside the coordinator's dataflow graph;
  routing the SMB campaign end-to-end through the controller; retiring the compat spine)
  is filed as **`hm-e6q`**, deliberately out of scope.

## Architecture

`explorer → {sdk-events, revision-coordinator, control-proto, environment}` — acyclic.
The `explorer ↔ sdk-events` cycle (sdk-events' `SdkEvent.moment` was `explorer::Moment`)
is broken by giving `sdk-events` a **local `Moment`** (conventions rule 2) and deleting
the compat that pulled in the Explorer's `Sensor`/`Oracle`/`GuestEvent`, so the Explorer
can now consume `sdk-events`' `Normalized` evidence.

The Differential runtime is driven **through** the merged `revision-coordinator`'s
`assign / complete / probe_drive / cohort` + `Ledger` seam. `ProbeHost` (the real
one-Timely-worker differential-dataflow host) is `pub(crate)` and currently an echo
harness (PR #124 F2 ruling); the observation/cell/occupancy **derivations are pure
functions of the barrier-passed committed evidence** — the strategy's "direct
recomputation is an oracle, not a second backend." Wiring the production relations into
`ProbeHost` is `hm-e6q`. The coordination guarantees this task depends on (crash-safe
revision ordering, the two probe barriers, canonical consolidated reads, cohort-atomic
visibility, crash recovery) are all the **real** coordinator.

## Acceptance-criterion invariant → test

| Invariant | Where | Test |
|---|---|---|
| Two-barrier protocol in one step (append → commit → barrier 1 → dedupe/order/cap → charge budget → hold seal → later revision → barrier 2 → keep-if-occupied) | `campaign.rs::step` | `campaign::tests::one_step_runs_the_two_barrier_protocol` |
| Restart rebuilds canonical inputs from the ledger + referenced payloads | `ledger.rs`, `campaign.rs` | `ledger::tests::append_survives_reopen`, `campaign::tests::restart_rebuilds_canonical_inputs_from_the_ledger` |
| Partial/uncommitted batches cannot advance a frontier | coordinator seam | `campaign::tests::an_uncommitted_batch_cannot_advance_the_frontier` |
| TraceStore retention cannot delete a live reference | `ledger.rs::TraceStore::retain` | `ledger::tests::retention_cannot_delete_a_live_reference` |
| No provisional transition occupies the archive | `campaign.rs::provisional_candidates` (nominate-only) | `campaign::tests::no_provisional_transition_occupies_the_archive` |
| Disappearing pre-seal state not admitted; evidence at/after a boundary can't influence an earlier cell (absorbs `hm-mcx`) | half-open included-count cut, `evidence.rs::reduce_at_cut` | `campaign::tests::evidence_after_the_seal_cannot_influence_an_earlier_cell` |
| CellFn at the actual server-captured `sealed_at` (included-count prefix, never a Moment) | `evidence.rs::reduce_at_cut` + `ObservationCells` | `evidence::tests::set_reduces_to_latest_within_the_half_open_prefix` |
| Independent per-observation reduction (`set`/`max`/`min`/`accumulate`), no packed feature | `evidence.rs::reduce_at_cut` | `evidence::tests::{max_and_min…, accumulate…, independent_registers…}` |
| Deterministic best-Entry-per-cell occupancy; quality domination + stable tie-break; Entry eviction separate from evidence retention | `campaign.rs::Occupancy` | `campaign::tests::occupancy_keeps_the_best_entry_per_cell` |
| Binary-terminal & JSON occurrence counterexamples over the immutable view, deduped by property; site coverage separate | `occurrence.rs::OccurrenceOracle` | `occurrence::tests::{json_always_false…, multiple_sites…dedup…, binary_terminal…, binary_unreachable…}`, `campaign::tests::controller_reports_occurrence_counterexamples_once` |
| `sometimes`/`reachable` absence over finalized property aggregates, retention-stable counts | `occurrence.rs::AbsenceLedger` | `occurrence::tests::never_satisfied_sometimes_is_a_retention_stable_absence` |
| Branch ingestion appends only the child suffix (inherited prefix never duplicated) | `campaign.rs::decode_child_suffix` | exercised by the exploit path; the suffix slice is unit-covered by the cut semantics |
| Legacy `LinkSensor`/LINK/packed `FeatureId` deleted | `sdk-events` | crate deleted; public-api snapshot regenerated |
| End-to-end same-seed artifacts identical | `campaign.rs` | `campaign::tests::same_seed_yields_identical_campaign` |

## Notes / judgment calls for the reviewer

- **Cut semantics.** `EvidenceCut.sdk_events` is the **SDK-event vector prefix length** (a
  count of included `SdkEvent`s), and `reduce_at_cut` includes the first `included` events.
  This is deliberately **not** the catalog-gapped `SdkEvent.ordinal` (the schema
  declaration is not an event and occupies no cut position) and never a `Moment`
  comparison. The scripted test machine stamps the count consistently.
- **Occurrence Oracle role.** The retired `AlwaysViolation` decoder-crate type is
  subsumed by `OccurrenceOracle` (the `CounterexampleKind::TerminalAssertion` arm) — the
  strategy's "the Oracle role is not redundant" made concrete in the Explorer layer.
- **Legacy path "unreachable."** The production campaign path is `DifferentialCampaign`,
  which never touches `Archive::admit`. `campaign-runner`'s campaigns never used
  `Explorer::step`/`Archive::admit` either (they hand-roll over `Machine`). The compat
  engine (`Explorer::step` + the spine) remains for the DSL consumers and its own
  behavior-equivalence tests; physically retiring it rides `hm-e6q`.
- **Serial-console adapter not promoted** (per the task notes): it is source-local,
  stop-granular, full-run-only, and cannot drive exact same-Moment cuts, cross-source
  sequences, or log-derived CellFn dimensions at `sealed_at`. Promotion needs capture-time
  serial stamps + a snapshot cursor + a shared machine-event ordinal — out of scope.

## What ran where

All portable gates ran **laptop-side (macOS)**: `cargo build`, `cargo nextest`,
`cargo clippy --all-targets -D warnings`, `cargo fmt --check`, regenerated `cargo
public-api` snapshots, and `cargo deny check`, for `explorer`, `sdk-events`, and
`campaign-runner`. The same-seed-identical-artifacts determinism property runs
laptop-side over the in-crate scripted machine. **The box KVM determinism re-run (the SMB
25/25 `state_hash` gate + film) is handed to the foreman** per Paul's ruling — the game
cell re-sourcing is byte-identical by construction (`campaign-runner`'s
`campaign_replays_bit_identically` pins it laptop-side), so the box run is a confirmation,
not a blocker.
