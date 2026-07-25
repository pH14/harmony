# Task 158 — W7: PR #134 evidence + campaign bookkeeping hardening (hm-zduj)

Branch `task/campaign-evidence-hardening`, one commit per bead (one landed
pair). This file is the review record / PR-body draft; the per-commit
messages carry the same content in shorter form.

## Beads → commits

| Bead | Commit | Outcome |
|------|--------|---------|
| hm-tx66 (F7) | `4a66714d` | Fixed — one validated staging choke point |
| hm-t5py | `e61e364c` | Fixed — checked catalog-cut shift (separate guard) |
| hm-9zr5 (F5) | `1a8fca30` (note) | **Already resolved** by the v3/v4 bumps; evidence below |
| hm-8deo (V10) | `1a8fca30` | Fixed — full-config `CampaignConfigId` |
| hm-vfop (F6) | `345f8816` + `2ac9ae9b` | Fixed — fresh-dir enforced precondition (+ in-diff-mutants hardening) |
| hm-dd39 (V11) | `347374f1` | Fixed — upgraded catalogs embed the raw v1 bytes |
| hm-7h2c (V9) | `5a4af893` | Fixed — recovery refuses unaccounted batches |
| hm-7k8f (F4) | `115693d7` | Fixed — full-view parity oracle + planted failures |
| hm-w2ar | `201f4fb6` | Boundary restated + tested; mutant was **equivalent**; timeouts triaged |
| hm-4vms (F9) | — | **Left open by design** (standing escalation); diagnostics untouched |

## 1. hm-tx66 — the choke point

`Coordinator::stage_evidence` is now the single validated entry for
relation rows:

- **Lineage acyclicity.** A self-parent or a cycle closing through the
  staged/fed edge set is a typed `CoordError::LineageCycle`. Previously the
  edge reached the dataflow, whose distinct-over-depth ancestry iteration
  never converges — `probe_drive` **hung the process** (the F7 hang) instead
  of erring. Regression tests stage both the length-one and the two-edge
  cycle.
- **Intra-batch declare conflicts.** One identity under two ops *within one
  batch* is now `DeclarationConflict` (the pair used to slip past the
  cross-batch map, and feed-time dedup then silently dropped the second).
  Exact same-op duplicates stay idempotent.
- **Structural bounds** (`CoordError::EvidenceRowsInvalid`): non-monotone
  state-event positions (the reduce contract feeds each coordinate exactly
  once; a duplicate only ever surfaced as a debug-build multiplicity
  assert), positions/cut counts below the batch's own lineage fork count,
  duplicate provisional-cut counts, and an Entry offer with no seal row
  (previously dropped *silently* by `feed_rows`).
- **Checked cumulative positions, one invariant.** The evidence ledger's
  ingest (append **and** replay) refuses a batch whose `parent_cut` base +
  own event count overflows u64 (`LedgerError::PositionOverflow`), so every
  downstream `start + i` (staging, composition) is in-range **by
  invariant**. The campaign step computes its observed cut with
  `checked_add` against the same typed error, and
  `compose_observations_at` saturates as documented defense-in-depth
  (saturation fail-closed *excludes*; it can never wrap to position 0 and
  double-count).
- **Frame bound at append.** `append_record` refuses a record larger than
  the replay bound (`LedgerError::OversizedRecord`): the file can never
  carry a frame its own replay rejects on the next open. (Tested on the
  pure bound — a real >64 MiB record would dominate suite runtime.)
- **The O(N²) accumulated-prefix clone** in the `cum` reduce is gone:
  combine from `vec.last()` instead of keeping and re-cloning a second
  running aggregate (one stored aggregate per boundary — the inherent
  output — instead of two copies per step).

**hm-t5py** is the same treatment at a seam the choke point cannot reach
(the `DeclaredMachine` transport boundary in campaign-runner), so it is the
separate guard the spec allows: the prepended-catalog cut shift is
`checked_add`, refusing a `u64::MAX` stamp as the transport-class error it
is (previously: debug panic / release wrap-to-zero, and a wrapped cut
retains the whole inherited prefix as child evidence). Both boundary sides
pinned.

## 2. hm-9zr5 + hm-8deo — the on-disk identity pair

**hm-9zr5 is already resolved — no new bump, with evidence.** The finding
was real at filing: PR #134 changed `canonical_bytes()`/`EvidenceBatchId`
(serde-default `parent_cut` + `sealable_moments`) while the ledger stayed
v2. Since then the format bumped twice: v3 (task 144, `hm-j7ie`) and v4
(task 152, `hm-mmkf`). Every pre-4 ledger — which includes **every**
pre-patch v2 artifact this bead describes — is refused loudly
(`LedgerError::UnsupportedVersion`), never silently recomputed.

*What a v2/v3 holder loses, and what the message tells them:* the ledger
refuses to open, naming the found version, this build's version, and the
current semantic boundary (the hm-mmkf checkpoint verdict-fold change); no
read-old or migration path exists, so the artifact's batches, checkpoints,
and tombstones are unreadable by this build — re-run the campaign to
regenerate. Pinned by four existing tests (`foreign_version_is_rejected`,
`version_two_ledger_is_refused_with_the_fold_semantics_reason`,
`version_three_ledger_is_refused_with_the_fold_semantics_reason`,
`future_version_is_rejected_without_the_fold_semantics_claim`) — the gate's
demanded "test proving a v2 ledger is rejected with the intended message"
already ships.

**hm-8deo.** `run_game_campaign` pinned `CampaignConfigId::digest(seed)` —
same-seed runs under different selectors/deadlines/caps collided on one
durable identity, violating the `ids.rs` contract. Now
`GameCampaignConfig::config_id(config)` digests a canonical **versioned**
encoding of the full configuration plus the `ExplorationConfig`. The
host-local `trace_dir` is excluded by design (output placement, not
campaign semantics — including it would give one campaign different
identities on different hosts); the leading version byte covers the
encoding *and* the driver-implied constants.

*Ordering-hazard note (why no bump rides this):* the `CampaignConfigId`
lands only in the coordinator ledger's genesis record, and every shipped
`run_game_campaign` coordinator is a `MemLedger` — no durable artifact
holds a seed-only identity this change could silently reinterpret, and no
identity *function* on the evidence ledger changes. `mazecampaign.rs:769`
still mints a seed-only id — out of this work order's surface; flagged for
a follow-up bead rather than smuggled in.

## 3. hm-vfop — reused `--trace-out`

The fresh-dir requirement is now an enforced precondition with the fix in
the message: any durable content under `trace_dir/evidence.log` (retained
batches, a checkpoint, or the finalized end) is a typed
`GameCampaignError::TraceDirNotFresh` — "pass a fresh --trace-out directory
(or remove the old evidence.log) per run" — instead of the confusing
first-step `OccupancyDivergence` the reopened ledger used to produce
against an always-fresh `MemLedger` coordinator. Tombstones need no
condition of their own: `collect` demands durable coverage (a checkpoint
or the finalized end) before writing one, so a tombstone-bearing ledger
always trips a checked condition — the first in-diff mutants pass flagged
the original redundant disjunct, and the follow-up commit (`2ac9ae9b`)
dropped it and made each remaining condition independently decisive
(checkpoint-only and finalized-only regression legs). The box driver's
per-rep `rep-N/` isolation already satisfies the precondition (pinned in
the regression). **Distinct from hm-4vms**: that standing intra-campaign
anomaly's diagnostics (`check_occupancy`'s divergence detail) are
untouched and stay armed; this closes only the cross-invocation reuse
confusion.

## 4. hm-dd39 — the audit promise

`resolve_v1_declaration` now emits an **upgraded catalog wire form**
(version byte `0x82`, host-minted only): the resolved v2 records plus the
original guest v1 declaration bytes embedded verbatim. The decoder
validates the embedded original against the v2 records (identity set,
classification, expectation, name — a swapped, drifted, or nested
provenance is a typed `SdkError::UpgradeProvenance` refusal), and
`SdkSchema::original_v1_declaration()` recovers the exact guest bytes from
the persisted artifact, surviving the `Normalized` serde round trip. The
schema's audit promise is **true again by construction** — the provenance
rides inside the recorded declaration, so the self-validating
redecode-and-compare load path needs no second channel. Decode *semantics*
are unchanged from the plain-v2 upgrade (v1 assertion verbs stay
normalized away; the embedded bytes change nothing at decode).

*Identity note:* newly-recorded captures from v1-declaring guests carry
different catalog bytes than the pre-fix build would have produced, so
same-seed `EvidenceBatchId`s differ across the upgrade **for that guest
class only**. No identity function changes and no existing durable record
is reinterpreted (old plain-v2 blobs still decode) — no ledger bump.

## 5. hm-7h2c — recovery accounting

`DifferentialCampaign::new`'s recovery re-staging no longer silently skips
a committed batch `ledger.get()` cannot return. Every batch the
coordinator durably ordered must be accounted for: retained → re-staged;
collected → its tombstone vouches and it contributes nothing (mirroring
the retention rebuild); anything else → typed
`CampaignError::RecoveryIncomplete`. The old "foreign coordinator's input"
tolerance was exactly the silent-omission species this work order kills.
The regression drives all three legs over a real `FileLedger`-recovered
coordinator: empty ledger refuses; the true reopened ledger resumes and
keeps stepping; a batch collected under a covering checkpoint passes.

## 6. hm-7k8f — the widened parity oracle

`assert_view_parity` filtered materialized rows down to ledger-predicted
coordinates, so a phantom row at a **never-staged** coordinate was
structurally invisible. The oracle now recomputes the complete expected
observation/cell/occupancy views and requires whole-vector equality
(two-sided: extra *and* dropped rows diverge), through a
`Result`-returning core. Per W1 doctrine the planted-failure fixture ships
with it: phantom observation/cell rows at a never-staged coordinate, a
phantom occupancy row, and a dropped row each make the oracle go red while
the real views pass. Note the widened oracle passing over every existing
campaign suite is itself evidence: the production relations materialize
*no* rows the ledger recompute does not predict.

## 7. hm-w2ar — the mutant and the timeouts

The missed mutant (`host.rs:471` `>`→`>=` in `ProbeHost::drive`, run
29857330385 shard 3) is **semantically equivalent**: the guard's equal case
assigns an equal value to the monotone watermark, so no suite can
distinguish it — an *untestable* boundary, not an untested one. Resolution:
the watermark is restated as the `max` it is (removing the
equivalent-mutant site) and a direct monotonicity unit test pins the
boundary (lower drive never regresses, equal holds exactly, higher
advances), killing the regression-class mutants (min-swap, dropped
assignment) at the source.

Timeout triage (5, none a slow-test artifact):

- **Shard 3 (3):** `drain_ready → vec![]`, `advance_flush → ()`,
  `advance → ()` — one class: suppressed frontier advancement spins
  `probe_drive`'s wait-for-the-probe loop forever. That hang is the
  documented defensive posture of the probe barrier (`drive`'s own
  comment); the timeout **is** the kill.
- **Shard 2 (2):** `&&`→`||` and `==`→`!=` inside
  `compose_observations_at`'s run-forward-suffix seal lookup — the
  corrupted match predicate composes wrong/huge segments and the 256-case
  proptest parity suites balloon past the auto 20 s timeout;
  resource-exhaustion kills of real corruption.

## 8. hm-4vms — untouched, by design

The standing unreproduced occupancy divergence stays open. Nothing in this
task disarms or reroutes its diagnostics: `check_occupancy` and its
divergence detail strings are unchanged; the hm-vfop precondition removes
only the *cross-invocation* false positive, which the bead itself
distinguishes from the intra-campaign anomaly. It did not reproduce during
this task's runs (full workspace suite ×, gamecampaign smoke campaigns ×).

## Gates

- `cargo nextest run --workspace --all-features`: **2149 passed** (33
  skipped: box-only/#[ignore]).
- `cargo clippy --workspace --all-features --all-targets -- -D warnings`:
  clean. `cargo fmt --all -- --check`: clean. `cargo deny check`: clean.
- Public-API snapshots regenerated on the pinned nightly
  (`nightly-2026-06-16`, cargo-public-api 0.52.0) for all four touched
  crates — deltas are purely additive: new error variants
  (`CoordError::{LineageCycle, EvidenceRowsInvalid}`,
  `LedgerError::{PositionOverflow, OversizedRecord}`,
  `CampaignError::RecoveryIncomplete`, `SdkError::UpgradeProvenance`,
  `GameCampaignError::TraceDirNotFresh`) and two additive methods
  (`GameCampaignConfig::config_id`,
  `SdkSchema::original_v1_declaration`).
- `cargo mutants --no-shuffle --in-diff` over the full task diff (final
  tree, `main...HEAD`): **66 mutants tested in 16m: 60 caught, 6 unviable,
  0 missed, 0 timeouts.** The 6 unviable are `Default::default()`
  return-replacements on types with no `Default` impl — structurally
  unbuildable, not gaps. Two earlier passes each caught real weaknesses in
  this task's own tests, fixed in `2ac9ae9b` (vfop guard disjuncts made
  independently decisive; the redundant tombstone disjunct removed with
  proof) and `d47f8212` (fork-count equality boundary pinned;
  expectation-only provenance-drift leg added — the only reachable way to
  make that comparison disjunct decisive, since a v1 catalog's namespace
  fixes its classification).

## Judgment calls / limitations for the integrator

- **Coordinator lineage map is append-only and process-local** (rebuilt on
  recovery via re-staging). A hostile double-stage of one rollout under
  two proposals with different parents overwrites the edge; the walk stays
  acyclic-checked at each staging, so the hang stays unreachable — noted
  rather than defended further (the ledger's own ingest cycle refusal,
  hm-wjv1, is the durable authority).
- **`OversizedRecord` is tested on the pure bound**, not through a real
  >64 MiB append (runtime). The framer calls the helper on every append.
- **mazecampaign's seed-only config id** is out of surface and still
  present — a candidate follow-up bead, deliberately not smuggled in.
- The **hm-dd39 wire byte `0x82`** is host-minted only; the guest version
  sequence (1, 2) is untouched, so no guest-side change is needed.
