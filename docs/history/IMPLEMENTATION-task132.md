# Task 132 — Full Differential vertical (hm-e6q): implementation record

Branch `task/differential-vertical` (worker session 2026-07-17/18). This is the
frontier-task write-up (conventions: multi-crate task, review record) — kept
in-repo because it carries the box runbook and gate evidence the acceptance
criteria cite. Acceptance mapping at the end.

## M1 — Production DD relations inside `ProbeHost`

**revision-coordinator** (public surface additive-only; snapshot diff +113/−0):

- `relations.rs`: the payload-blind row vocabulary. Observation identities are
  opaque canonical bytes (`ObsKey`); cells are opaque bytes (`CellBytes`); the
  coordinator owns only the temporal semantics. Rows: `EvidenceRows { rollout,
  lineage, declares, events, obs_cuts, seal, entry }` with cumulative positions
  and half-open `CutRow`s; views: `MaterializedViews { observations, cells,
  occupancy }`. The `Agg` segment aggregate (Last/Max/Min/Distinct) is the
  spike's, with commutative/associative combine (tested).
- `host.rs`: the committed-input relation (the PR-#124 echo → `DrainedView`) is
  **byte-for-byte unchanged** — every prior coordinator test passes unmodified,
  including the goldens and `spike_integration`. Beside it, the productionized
  `spikes/differential-lineage` shapes: ancestry by `iterate` over the standard
  `Product<u64, u64>` (no custom lattice; Revision is the only timestamp;
  branch is a key), the **shared segment-aggregate formulation** (boundary
  vectors include child fork counts so cumulative lookups are exact), the cell
  projection evaluated in-graph at every evaluation point (the seed carries the
  cut; empty maps included), and best-entry-per-cell occupancy (quality desc,
  entry-id asc) over committed Entry offers at **seal** cells only —
  provisional cuts are structurally unable to reach occupancy.
- `Coordinator` additions: `stage_evidence` (idempotent restage;
  `StageConflict`/`StagedTooLate`/`DeclarationConflict` typed errors; declares
  deduped before feeding so declaration joins never fan out),
  `set_cell_projection` (pre-drain only; `ProjectionTooLate`), `materialized`
  (probe-barrier + visible-frontier read discipline, `FrontierStalled` on a
  premature read), `committed_inputs` (the recovery re-staging hook — restart
  replays committed ledger inputs, never a live arrangement).
- `tests/relations.rs`: half-open reduction for all four ops, lineage prefix
  composition (ancestor override/union/inherit), occupancy domination + ties,
  the staging discipline, the read barrier, a custom projection, and
  live-vs-recovered byte-identical views.

**explorer** — the controller now *reads* the plane instead of recomputing:

- `DifferentialCampaign::new` installs the campaign's `ObservationCells` as the
  coordinator's cell projection and re-stages committed inputs from the durable
  evidence ledger on recovery. `step()` stages typed rows at append time;
  barrier-1 candidates come from the materialized **cut** cells; barrier-2
  admission reads the materialized **seal** cells; both are loud on a missing
  row (`ViewIncomplete`). After barrier 2 the operational archive is reconciled
  against the materialized occupancy view — `OccupancyDivergence` is a typed,
  loud error, never absorbed.
- `CompletedRunEvidence` gains `parent_cut` and `sealable_moments`
  (serde-defaulted), so the ledger alone re-derives byte-identical relation
  rows on restart. `RunId.parent` now records the parent **rollout issue**
  (matching its documentation; it previously held a process-local
  `ExemplarRef`).
- `compose_observations_at` is the **direct-recomputation ORACLE**
  ("an oracle, not a second backend"): position-aware lineage composition
  (ancestor segments through their fork cuts + own suffix; a pre-campaign
  setup prefix restored into the genesis base occupies positions below the
  genesis cut and belongs to no rollout batch). `RetentionViews::fold_batch`
  now takes the ledger and computes seal cells on the composed prefix, keeping
  record-3 assignments in lock-step with the Differential occupancy.
- **The M1 gate**: `materialized_views_match_direct_recomputation` and
  `lineage_views_match_direct_recomputation` assert view-for-view equality
  (observations, cells, occupancy) between the materialized views and the pure
  recomputation after every barrier-passed step — genesis multi-op and real
  exploit lineage (the testkit `ScriptedMachine` is now lineage-aware:
  snapshots capture the emitted prefix, branch restores it, child scripts are
  branch-relative — the cumulative-count contract).

Deviations considered and rejected (M1):
- Depending on `spikes/differential-lineage` as a production dependency —
  rejected; the relations are ported (spikes gate trust, not construction).
- Nominating provisional candidates from an in-graph transitions relation (the
  spike's `Transition`) — the controller's nomination rule is "unoccupied
  cell", which needs the operational frontier anyway; cut cells are
  materialized and the freshness filter stays imperative. Representable later
  without surface change.
- Working-set/property/absence relations in DD — deliberately NOT moved: the
  hm-5sv retention views are the ruled owner of records 2/3 and already rebuild
  from the ledger; duplicating them in-graph would mint a second authority.

## M2 — SMB through the two-barrier controller

`run_game_campaign` now constructs and drives `explorer::DifferentialCampaign`
(machine by value, `Box<dyn EnvCodec>`); the bespoke BTreeSet/Vec loop is gone.

- The workload pre-loop is unchanged: `setup_complete` base seal, billboard
  extraction/validation, ROM cross-check (boxrun). The machine then moves into
  the controller.
- `QuietCodec`: exploit-mutate is the reseed-only `quiet_mutate` — the
  controller structurally cannot mint a host-plane fault; a fault-carrying
  exemplar maps to the codec's fail-closed `UnsupportedComposition`.
- `SmbObservationCells`: the `(mode, world, level, x-bucket)` tuple over the
  independently-reduced observation map (empty cell before the first X_BUCKET).
- `DeclaredMachine` + `sdk_events::resolve_v1_declaration`: the **explicit
  workload instrumentation declaration** the strategy sanctions. The play-agent
  declares a wire-v1 catalog (unresolved state, correctly non-reducible); the
  wrapper upgrades that declaration **in place** to v2 with resolved base ops,
  through the decoder's own v1 parsing (classifications/expectations/names
  cannot drift). A guest with no catalog (the portable toys) gets the
  standalone declaration prepended. The resolution table must cover **every**
  declared state register (v2 cannot express unresolved state) — an incomplete
  table fails loudly.
- Controller knobs (additive): `Nomination::EventMoments` (provisional-cut
  coordinates from the rollout's own SDK-event moments, for a workload whose
  only machine snapshot point is its setup seal) and `hash_rollouts` /
  `StepReport::state_hash` (the per-branch determinism artifact, taken at the
  rollout terminal before any materialization replay).
- Controller fixes surfaced by the reroute: a candidate whose state disappears
  before a valid seal is **dropped** (the strategy's disappearing-state rule),
  and its revision slot is reserved only once the seal is held, so a skip can
  never hold the frontier open; genesis-rooted branches inherit the machine's
  pre-campaign SDK prefix through the genesis cut.
- The per-branch log (`DiscoveryEvent`: touched cells, depth, state_hash),
  `WorkEvidence` minima, the vacuity guard, `RolloutDied`, and deep-reproducer
  retention all derive from the committed evidence batches. A branch's V-time
  span is measured from its own start (the exploited Entry's cut for a child).
- PureRandom → `GenesisSelector`; SelectorV1 → `ExploreExploitSelector` with
  the configured explore period; Signal still refused loudly.
- Box-gate repetitions are **independent**: each gets its own trace/evidence
  directory (a shared durable ledger would seed rep N with rep N−1's committed
  assignments — resumption, not repetition).

### Smoke-fire-once findings (the ruled discipline, all fixed pre-gate)

The single-seed probe (4 branches, `--repeat 2`, real KVM) caught four real
integration defects before any campaign spend:

1. **Double catalog**: the real guest declares its own v1 catalog; injecting a
   second declaration tuple violates the one-catalog rule → the in-place
   upgrade (`resolve_v1_declaration`).
2. **Unresolved POWERUP**: the guest also declares register 5; wire v2 cannot
   express unresolved state, so the resolution table must cover the full
   register set (contract documented; POWERUP resolved as `set`).
3. **Missing trace dir**: the evidence ledger opened under a not-yet-created
   `--trace-out` directory.
4. **Shared-ledger repetitions**: rep 2 rebuilt rep 1's assignments into its
   mirror while its fresh coordinator held none — the occupancy-divergence
   check fired exactly as designed; fixed with per-rep isolation.

**Open observation (escalate-worthy):** one smoke attempt (same binary class,
fresh state) failed *intra-campaign* with `occupancy divergence: 2 materialized
cells vs 4 mirror claims`, and the immediately following identical invocation
passed clean. The divergence check is the loud surface for exactly this class;
diagnostics (both-side cell dumps via `Frontier::claims`) are now armed. If it
recurs in any gate the run fails loudly and the dump identifies the drifting
side. It did not recur across the 27 box repetitions below — the 25/25 determinism
gate (25 fresh-boot reps) plus the smoke-fire-once (2 reps). This should be
treated as an unexplained nondeterminism signal until root-caused, per the
divergence-is-P0 discipline (tracked as `hm-4vms`).

### Box gate evidence (determinism 25/25 + film)

Runbook (box `hetzner`, worktree `/root/harmony-t132` provisioned from a git
bundle; patched-KVM window + leased core via
`bash scripts/box-window.sh acquire t132`; guest artifacts from the task-86
build; ROM `/root/roms/smb.nes`, sha256
`0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`):

```sh
taskset -c 2 ./target/release/campaign-runner game box \
  --config pure-random --campaign-seed 1 --max-branches 8 \
  --deadline-delta 2000000000 --repeat 25 \
  --rom-sha256 0b3d…6dea --logs-out … --trace-out …
HARMONY_SMB_CORE=guest/build/fceumm_libretro.so HARMONY_SMB_ROM=/root/roms/smb.nes \
FILM_OUT_DIR=… taskset -c 2 cargo test -p campaign-runner --release \
  --test live_film -- --ignored --nocapture --test-threads=1
```

RESULTS (2026-07-18, box `hetzner`, patched-KVM window, leased core 2, commit
`c99759af`):

- **Smoke-fire-once** (4 branches, `--repeat 2`): PASS — record + replay
  bit-identical on real KVM (`SMOKE_EXIT=0`), after the four findings above.
- **Determinism gate**: `game box DETERMINISM PASS: 25/25 identical
  per-branch state_hash sequences (gate floor 25)` — fresh boot per
  repetition, PureRandom seed 1, 8 branches, 2 s of V-time per rollout,
  through the full two-barrier path (evidence append → coordinator commit →
  barrier-1 materialized cut cells → budgeted materialization replay →
  barrier-2 materialized seal cells + occupancy reconciliation).
- **Work evidence** (vacuity guard satisfied): 8 branches, weakest rollout
  2 000 000 064 ns of V-time / 4 976 COMPLETED frames.
- **Billboard window** (film's input, from the setup prefix):
  `gpa=0x4e00000 len=15838`.
- **Deep reproducer** retained per repetition (content-addressed; e.g. rep
  trace `b6946502…e1d2`, branch 4, depth 1 — the small-budget PureRandom
  depth plateau matches the M0 baseline record).
- **Film gate** (task-87 projector path, `tests/live_film.rs`): PASS
  (`FILM_GATE_EXIT=0`) — 383 exact `(frame, moment)` pairs calibrated, plan
  of 300 frames, unfilmed terminal hash stable 25/25, **hash-neutrality
  25/25 (filmed == unfilmed)**, render determinism (300 frames + contact
  sheet, twice, identical), sheet blake3
  `82aea5ca5a19ee88163aea10e633f388babc9a78ac74afa76ca444fbaffc3b17`;
  artifacts under `/root/t132-film` on the box (bundle.json + 300 PPMs +
  contact.ppm); a downscaled contact-sheet preview was delivered for visual
  inspection. (One environmental retry: `HARMONY_SMB_CORE` must be an
  absolute path — `cargo test` runs from the crate dir; the first attempt's
  deterministic half passed identically, the render half refused the
  relative core path loudly.)

RE-CERTIFICATION (2026-07-21, box `hetzner`, patched-KVM window, leased core 2,
final head `0280ec5f` — the tribunal VERIFY-event **V1** fix). V1 corrected the
`DeclaredMachine` seal cut to be catalog-inclusive, which changes the portable
game path's evidence **cells/lineage** (it drops the previously-retained stray
inherited firing). Because the box evidence above predates the fix, the gate was
re-fired on the fixed head:

- **Smoke-fire-once** (4 branches, `--repeat 2`): PASS — 2/2 record→replay
  bit-identical on real KVM (`SMOKE_EXIT=0`).
- **Determinism gate**: `game box DETERMINISM PASS: 25/25 identical per-branch
  state_hash sequences (gate floor 25)` — PureRandom seed 1, 8 branches, 2 s of
  V-time per rollout, full two-barrier path. Work evidence: weakest rollout
  2 000 000 064 ns / 4 977 COMPLETED frames. Billboard `gpa=0x4e00000 len=15838`.
- **The fix is surgical**: the deep reproducer of the deepest branch is trace
  `b6946502…e1d2` (branch 4, depth 1) — **byte-identical to the pre-fix run
  above**. The guest execution and its frame journal are unchanged; only the
  SDK-cut/cell accounting moved. So the **film gate is unaffected** (it renders
  from the guest frame journal, not the SDK cut), and the 2026-07-18 film cert
  stands without re-run.

## M3 — Legacy spine retired (physically removed)

- **explorer**: `engine.rs` (`Explorer`/`Composition`/`RunOutcome` — the
  legacy `Explorer::step` loop and the only `Archive::admit` caller) deleted.
  The spine loses `Archive`, `Sensor`, `CellFn`, `Feature`, `FeatureId`,
  `FeatureSet`, `ChannelId`, `Fork`; defaults lose `CoverageArchive`,
  `IdentityCells`, `COVERAGE_CHANNEL` and the AFL bucket/coverage featurizers.
  Surviving (per the strategy's "fate of the spine interfaces"): `Tactic`,
  `Selector`, `Oracle`, `Matchable`, the archive read model
  (`Frontier`/`FrontierEntry`/`ExemplarRef`/`VirtualExemplar`/`CellKey`/
  `Reward`), the evidence vocabulary (`RunTrace`, `Record`, `StreamId`,
  `GuestEvent`, `CoverageView`, `Moment`, `EvidenceCut`), the default policies
  (`DeclineTactic`/`GenesisSelector`/`ExploreExploitSelector`), and
  `TerminalOracle` + the pinned bug fingerprint — **kept, not deleted**: they
  are production-live via campaign-runner's `CrashOracle`, and the spec's
  deletion list does not name them.
- **Consumers migrated** (crate-local vocabulary, conventions rule 2): logtmpl
  gains its own `ChannelId`/`Feature`/`FeatureId`/`FeatureSet`;
  `LogSensor::observe` and `CellFnV1::key` become inherent methods (algorithms
  byte-identical; the codebook/encoding gates all pass unmodified). matcher
  gains its own `ChannelId`/`Feature`/`FeatureId`; `MatchSensor::observe`
  inherent; `MatchOracle` keeps `impl Oracle`. benchcampaign consumes logtmpl's
  types directly; the runtrace test sensor uses a local pair type.
- **Test coverage disposition**: the engine-behavior suites retired with the
  engine (`behavior_equiv` + the vendored pre-refactor reference,
  `engine_pins`, `novelty`, `smoke`, `errors`, `determinism`,
  `artifact_equiv`, `spine_invariants`, `gc`, `replay`).
  `materialization.rs` keeps its direct-`Materializer` half (compose-fold
  chains, `NotSealable`/divergence/cut-divergence loudness) — the
  `Materializer` survives and stays covered; the `DifferentialCampaign` path
  carries its own determinism, materialization, and parity gates, and the box
  25/25 gate is the end-to-end replay evidence. `RunTrace` and the `runtrace`
  store are NOT in the deletion list and remain the film/trace currency.

## Gates: what ran where

- **Mac (portable)**: full workspace `cargo nextest` (2025+ tests), `cargo
  clippy --workspace --all-features --all-targets -D warnings`, `cargo fmt
  --check`, `cargo deny check`, public-api snapshots regenerated
  (revision-coordinator additive-only; explorer −92 deleted legacy items;
  logtmpl/matcher local-vocab; sdk-events +1; campaign-runner reroute).
- **Mac (mutation)**: `cargo mutants --in-diff` over the full branch diff
  (270 mutants; first pass 63 missed / 173 caught / 29 unviable / 5
  timeouts), then killer batches + `--iterate` verification passes. Killers
  added for every survivor in changed **production** logic: the
  instrumentation catalog/resolution (full-register coverage, per-op pins),
  `resolve_v1_declaration` (sdk-events-local: named-identity resolution,
  uncovered-state refusal), `DeclaredMachine` (upgrade-in-place, delegation
  counters, console), `SmbObservationCells` (tuple key, empty-cell,
  non-scalar), `QuietCodec` refusal classes, the occupancy-reconciliation
  tamper test (kills the `check_occupancy` stub), the cumulative-cut
  arithmetic + fork-truncation pins, the observation-id decode roundtrip,
  `Frontier::claims`, and the re-homed `FeatureSet` pins. **Accepted
  survivors** (documented, not silenced): 20 × `GameToyMachine` internal
  arithmetic (a self-consistent test double — both sides of every outcome
  comparison mutate together; pinning its trajectory bytes would test the
  toy, not the product) and 1 equivalent mutant (`ProbeHost::drive`'s
  monotone watermark `>` → `>=` assigns an equal value — a no-op). Final
  verification: no missed mutants outside those two classes.
- **Box (hetzner, patched KVM, leased core 2)**: smoke-fire-once probe
  (record+replay bit-identity, SMOKE_EXIT=0), determinism 25/25 gate, film
  gate — evidence above.
- **Not run here**: Linux CI quality jobs (coverage/Kani/Miri run in CI); the
  Miri job's `-p` list is unchanged (no new `unsafe`).

## Acceptance criteria → evidence

1. *Production DD relations run inside ProbeHost (occupancy/cells computed by
   differential-dataflow, not recomputed in explorer)* → M1 above; the
   controller reads `materialized()` views for candidates, admission cells,
   and occupancy reconciliation; the parity gates + `tests/relations.rs`.
2. *SMB campaign drives DifferentialCampaign end-to-end with box 25/25
   determinism green* → M2 above; box evidence section.
3. *Legacy `Archive::admit` path physically removed after the DSL consumers
   migrate* → M3 above (`engine.rs` + compat spine deleted; logtmpl/matcher/
   benchcampaign/runtrace migrated).

## Known limitations / integrator notes

- The evidence-ledger batch digests changed (`CompletedRunEvidence` gained two
  fields): pre-existing ledgers (test artifacts only) do not replay into the
  new digests. serde-defaulted decode keeps old files readable.
- `compose_observations_at` composes over the *retained* prefix: a collected
  (GC'd) ancestor contributes nothing. The retention rules make that reachable
  only behind a covering checkpoint; a stricter loud-on-missing-ancestor
  variant is a follow-up candidate.
- The intra-campaign occupancy-divergence anomaly (one occurrence, not
  reproduced) is flagged above for escalation.
- The film gate is unchanged by this task (it drives the projector path via
  `resolution::SocketServer`, not `run_game_campaign`); it re-ran as the M2
  spec requires.
- The coordinator's `Coordinator` is now `!Send` in practice (Rc-based cell
  projection); it already embedded a single-threaded Timely worker.
