# Implementation map and qualification

## Find the current contracts

These are the current paths; use `rg --files` and `rg` to find the owning
interfaces if the repository has moved them. Architecture plans and old
transcripts are not executable APIs.

| Responsibility | Existing code to inspect |
|---|---|
| Snapshot/branch/replay/run/read contract | `workloads/nes-machine/src/lib.rs` |
| Controller masks, duration bounds, reproducer encoding | `workloads/nes-machine/src/nes.rs` |
| Pinned core, RAM access, capture, snapshot compatibility | `workloads/nes-machine/src/quicknes.rs` |
| Smaller action/observation/snapshot interface | `dissonance/searcher/src/target.rs` |
| Typed execution, input, evaluation and reporting contracts | `dissonance/searcher/src/search/campaign.rs`, `CampaignTypes`, `TargetExecution`, `InputPolicy`, `Evaluation`, `Reporting` |
| Shared rollout and coordinator | `dissonance/searcher/src/search/rollout.rs`, `dissonance/searcher/src/search/campaign.rs` |
| Key groups, progress and preference, retention | `dissonance/searcher/src/search/archive.rs`, `ArchiveKey` |
| Mature campaign identity, recording, policy resolution | `workloads/nes/src/smb/campaign.rs` |
| Source-grounded decoder and native execution adapter | `workloads/nes/src/nova/target.rs` and nearby README |
| Spatial key and same-location preference | `workloads/nes/src/nova/archive.rs` |
| CLI run/replay/report/capture pattern | `workloads/nes/src/bin/nova-campaign.rs` |
| Multi-control game examples, if present | `workloads/nes/src/mm2/{target,archive,campaign}.rs` |
| Native/Consonance equivalence, when requested | `workloads/nes-machine/src/consonance.rs`, `workloads/nes/src/bin/nes-backend-oracle.rs` |

Within the standalone `workloads/nes/` package, normally add
`src/<game>/{mod,target,archive,campaign}.rs`, expose the module in `src/lib.rs`,
and add `src/bin/<game>-campaign.rs` and a small probe. Inspect
Cargo's current binary/feature discovery before modifying manifests.

Reuse the example's infrastructure, not its addresses, predicates, key order,
setup shortcuts, or policy names. Define associated types in `CampaignTypes`,
then implement the four contracts; `Game` is composed automatically. Prefer the
default shared rollout and preserve its restore/replay/suffix/probe ordering.
Check that it can represent exact event endpoints and non-admissible states;
if an existing `TargetExecution::execute_job` override is needed for correctness,
document why rather than weakening the observations to fit the shared loop.
Keep game code out of the generic searcher and machine drivers, and keep
scheduling in the existing coordinator.
The shared CLI package dispatch and backend oracle currently recognize specific
ROMs; registering a standalone campaign does not automatically add CLI or
Consonance support. Report those separately.
The existing `mm2` code may be concurrent/uncommitted; do not depend on it without
checking the task's checkout. Metroid may exist only on another branch.

## Contract checklist

- **Target:** construct one emulator per worker. Snapshots cross worker boundaries;
  thread-local emulator handles do not. Snapshot all future-affecting adapter
  state and clear stale action observations on reset/restore.
- **Actions:** use the shared NES encoding. Document held versus edge-triggered
  controls and deterministic duration distribution. Let the existing seeded
  policy draw them; keep learned policies distinct from handwritten macros.
- **Observation:** read every required RAM region (system RAM is not cartridge
  RAM), use checked lengths, and name the ROM revision supporting each address.
- **Terminal/evidence:** implement death and task completion distinctly. Merge
  interior observations without manufacturing a champion from incompatible
  maxima across different lineages. Infrastructure errors need their real cause.
- **Archive:** implement grouping, capacity, progress, and preference deliberately.
  Keep ancestry state bounded if `complete`/`record` need it. Canonicalize worker
  results before coordinator-dependent key completion.
- **Identity:** implement stream/checkpoint tags, ROM/core identity, and policy
  serialization/resolution. Reject unknown policy values and incompatible
  snapshots. An old recording must not silently acquire new semantics.
- **Memory:** supply conservative snapshot charges and bounded draw-state charges.
  Include adapter-owned histories/caches in the accounting; zero is justified
  only for actually absent state. Never retain an unbounded diagnostic history
  inside each snapshot.
- **Runner:** configure the existing campaign, record its origin and limits,
  and expose bounded run and replay paths. A stage runner or prefix input must
  identify the selected stage and full prefix provenance in its artifacts.

## Adapter README template

```markdown
# <Game> workload adapter

## Identity and origin
ROM revision and identity; external location or reproducible source build;
core identity/options; native or Consonance; normal start versus fixture;
setup controller tape and readiness predicate; opponent/AI and difficulty
configuration; any save edits or imports.

## Observations and search policy
| Field | Address/region or source symbol | Units/meaning | Role | Ordering/bucketing | Evidence |
| ... |

Input vocabulary and exclusions; press/release behavior; grouping/capacity;
preference tradeoffs; policy identifiers and known aliasing limitations.

## Outcomes
Death predicate; level/stage clear predicate; full-game ending predicate;
what is verified and what remains unknown. Progress-only if ending is unknown.

## Run and replay
Exact commands, artifact names, prerequisites, expected bounded smoke result.
Headless-to-film endpoint check and fixture/prefix reconstruction commands.

## Qualification
Checks and replay evidence, seeds and budgets, costs and achieved milestones;
known backend limitations and guidance deliberately encoded in this adapter.
```

## Validation ladder

1. **No ROM needed:** test decoding with synthetic memory and source-supported
   boundary cases. Check groups and comparisons with equal-count/different-item,
   backtracking, resource spending, menu, and transition cases where relevant.
   Run the portable library checks and the applicable current CI checks.
2. **Supplied ROM, short probes:** verify boot, controls, RAM regions, death and
   task terminal predicates. Unverified terminal predicates remain unknown.
   Snapshot S; run A; restore S; run A; compare observations and fingerprints.
   Then restore S; run B; restore S; run A; compare again. Where snapshots cross
   workers, repeat on a separately constructed target. Test an interior event
   and any retention probe's complete restoration.
3. **Small campaign:** start with hundreds to a few thousand executions on one
   or two available workers. Record stream/report/checkpoint, replay it, and
   compare decisions, evidence, and snapshot outputs using existing verifiers.
   Repeat the same logical configuration. Do not demand identical searches
   across different worker counts: worker count affects admission-window policy.
4. **Pilot:** increase a bounded budget after correctness checks pass. Publish
   progress over executions, frames, and search time; deaths/errors; archive
   size/charge and host RSS; output bytes. Film the champion and verify its
   decoded endpoint. An empty genesis or shortest retained tape is only a
   rendering smoke test; it cannot substitute for the searched champion or
   victory witness. If rendering the full tape is costly, retain full headless
   replay evidence and label the bounded excerpt precisely. Inspect real
   evidence before changing policy.
5. **Library evaluation:** register seeds, origin, success/progress measure, and
   limits before comparing engine changes. Include another game and a lower
   feasible memory budget. For performance work, follow the searcher README's
   worker sweep and mature-archive measurements. A stall is an informative
   result; a missing replay or false terminal is an integration defect.

Current portable checks are in `CONTRIBUTING.md` and the workload CI jobs.
For adapter changes, check the standalone NES package:

```sh
cargo fmt --manifest-path workloads/nes/Cargo.toml --all -- --check
cargo clippy --locked --manifest-path workloads/nes/Cargo.toml \
  --release --all-features --all-targets -- -D warnings
cargo test --locked --release --manifest-path workloads/nes/Cargo.toml --all-features
cargo deny --manifest-path workloads/nes/Cargo.toml check --config deny.toml
```

Also check `dissonance/Cargo.toml` when changing generic search contracts and
`workloads/nes-machine/Cargo.toml` when changing emulator drivers.
Use the repository toolchain. Changed unsafe logic additionally needs its
safety invariant documented and Miri coverage. State skipped ROM/hardware checks.
A documentation-only change does not call for emulator campaigns.

For a concrete runner pattern, Nova accepts:

```sh
cargo run --locked --release --manifest-path workloads/nes/Cargo.toml \
  --bin nova-campaign -- \
  --rom "$NOVA_ROM" --core "$QUICKNES_CORE" --output "$RUN_OUTPUT" \
  --seed 1 --executions 1000 --workers 1 --action-limit 512
```

The variables are operator-supplied paths. This runs Nova, not the new adapter;
verify a new runner's parser before documenting equivalent commands. Standard
Nova mode checks recorded replay; `--marketing-soak` omits that full campaign
verification and is unsuitable as its replacement. Its level-clear event is
not full-game completion. Later-level setup edits save state and is a fixture.

For CI, inspect `.github/workflows/nova-nightly.yml` and the pinned build scripts.
Separate portable synthetic tests from jobs requiring an external commercial
ROM. Copy neither a ROM nor a savestate containing its data into public output.
Use compact evidence for long runs; measure report/export cost separately from
time to the search milestone.
