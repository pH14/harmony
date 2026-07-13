# Task 105 — the vocabulary rename sweep (GLOSSARY ratification in code)

Bead `hm-u7q`. Binding authority: `docs/GLOSSARY.md` (Kills / Renames tables + consonance
addendum, as merged). One PR, mechanical-and-audited, provably behavior-neutral by
construction. This file is the implementation record; the section "The GLOSSARY table as
checklist" below is written to be lifted into the PR description.

## The GLOSSARY table as checklist

### Renames applied in this sweep

| Table row | Applied as | Commit |
|---|---|---|
| `dissonance/conductor` → **`campaign-runner`** (Kills + crates) | `git mv`, package + bin + `[conductor]` log prefix + campaign-data scripts + CI in lockstep | be66222 |
| `dissonance/link` → **`sdk-events`** (crates) | `git mv`, package, conductor dep, quality.yml `-p` list | 320b39f |
| `explorer::Environment` → **`Reproducer`** (types) | struct + all 7 dependents; the `environment::Environment` trait keeps its name (protected token in the pass); `EnvSpec`, `AdapterEnv`/`SeededEnv`/`RecordedEnv` stay | 5fa76cf |
| `VTime`/`Vtime` → **`Moment`/`Span`** (types, audited per use) | vmm-backend (all points → `Moment`); vmm-core `seal_rate` (targets/bounds/landings → `Moment`; widths/jitter/overshoots/depths/horizons/strides → `Span`); control-proto (merged into the existing `Moment` mirror — all points); environment (delays/skew → new `Span` newtype; `StandingFault::window` bounds → `(Moment, Moment)`); flow (stamps/due-times → `Moment`; `Latency` → new `Span`); explorer (merged into the spine's `Moment`) | ec1029b, cc7b543, 2c7b9d9, 5d12ba7, 1effc65, 5fa76cf |
| `unison::Machine`/`MachineFactory`/`MachineError` → **`Subject`/`SubjectFactory`/`SubjectError`** | trait triple only; impl names (`ToyMachine`, `FlakyMachine`, `CorpusMachine`) not ruled, keep; the spine's `Machine` trait keeps its name (GLOSSARY Keeps) | 06a8114 |
| `Modulation` → **rollout** (Kills) | `Explorer::modulation` → `rollout` + prose, surfaced by the explorer pass | 5fa76cf |
| `Progression`/`progression_step` → **the search loop / `step`** (Kills) | `Explorer::progression_step` → `step` + prose ("search-loop blindness" for invariant 5) | 5fa76cf |
| `CampaignOracle` → **`CrashOracle`** (Kills: "delete, or CrashOracle") | renamed, not deleted — see judgment calls | 5fa76cf |
| `vmm_backend::Event` → **`Injection`** (addendum types; "rides next touch") | surfaced by the vmm-backend `Vtime` audit — the same sequencing row this spec dispatches | ec1029b |
| "corpus GC" → **"pool GC"**, "Hypervizor VMM" → "the deterministic VMM" (addendum Kills) | doc comments across control.rs / explorer / control-proto / resolution / campaign-runner / hypercall-proto | 01c7c53 |
| Docs sweep (subordinate) | crate paths + type names in living docs (incl. the stale `counterpoint` crate reference in NESTED-INTEGRATION — the bead rules `campaign-runner`); historical records (GLOSSARY, QUEUE, REVIEW-2026-07, IMPLEMENTATION-task-\*, R-BACKEND spec snippets) left as records | 01c7c53 |

### Deferred, with the table's own sequencing as the reason

- **`Exemplar`/`VirtualExemplar`/`ExemplarRef`/`FrontierEntry` → `Entry` + `EntryRef`** —
  **`[question]`**, not applied. The table maps four legacy names onto two; `VirtualExemplar`
  and `FrontierEntry` are two distinct serialized structs (`FrontierEntry { exemplar:
  VirtualExemplar, env, reward }`), so the 4→2 mapping implies a structural merge, and the
  `exemplar` field name rides JSON artifacts — not achievable as a byte-neutral mechanical
  rename (landmine 1). Needs its own (format-versioned or shape-preserving) design pass.
- **`dissonance/runtrace` → `journal`, `logtmpl` → `log-templates`, `matcher` → `signals`,
  `tactics-regime` → `tactics`** — not in this spec's crate-rename scope (it names exactly
  two); GLOSSARY sequencing has each ride its crate's next substantive PR.
- **`det-corpus` → `acceptance-suite` (+ `Oracle` → `OracleKind`)** — addendum sequencing
  item 3 assigns it its own PR.
- **`vm-state` → `snapshot-state`** — addendum "cheap, anytime", but not dispatched here.
- **`vmcall-transport` → `hypercall-doorbell`** — explicitly rides task 43's window
  (guest-payload `MANIFEST.sha256` rebaseline).
- **`vmm-core` split names** — reserved for the ARM-backend window (addendum Reserved).
- **`Vtime`-prefixed state aggregates** (`VtimeState`, `VtimeWire`, `VtimeWiring`,
  `VtimeSnapshot`, `VmmError::Vtime`, `vtime::VtimeError`) — kept deliberately: the
  addendum retires `VTime` the *type*, and "V-time survives as the mechanism's name";
  these name the clock mechanism's state/wiring/errors, and `VtimeWire`'s fields ride the
  versioned `vm_state` binary blob. Same for the `vtime` crate itself and prose "V-time".
- **`gamecampaign::Vacuity::NoVTime`** — kept: names missing v-time *evidence* (mechanism
  word), and the variant name rides the campaign-report JSON (landmine 1).

## Landmines, each addressed

1. **Wire and serialized-artifact neutrality — ABSOLUTE.** No serde field/variant name was
   changed anywhere; renames are type/method/crate-level only, and every renamed newtype
   has the identical serde shape (`u64` newtype before and after). Golden coverage per
   artifact class, all green with checked-in fixtures byte-untouched (`git status` clean on
   every fixture): control wire (control-proto `golden.rs`), EnvSpec reproducer blob
   (environment `golden.rs`), flow action stream (flow `golden.rs`), journal/RunTrace
   store (runtrace store/version-bump suites + the committed `*.trace` fixtures),
   `vm_state` blob (vm-state untouched), campaign recordings (recording tests over the
   committed `mock_recording.trace`), explorer serde artifacts (`artifact_equiv.rs`). No
   class lacked a golden, so none were added.
2. **Hash-neutrality.** The exact-hash suites pass unchanged: explorer `engine_pins.rs`
   (pinned engine-stream hashes), campaign-runner `campaign_replays_bit_identically`,
   `determinism_proptest` (branch/run/replay hash pins), benchmark trigger pins — same
   pinned constants, zero fixture edits, across game + bench + planted campaign paths.
   The only same-seed stdout difference is the operator log prefix `[conductor]` →
   `[campaign-runner]` (renamed with the binary; grep sites in the campaign-data scripts
   updated in the same commit; the prefix reaches no recorded artifact and no hash).
3. **CI lockstep.** nightly.yml's Miri step now runs `-p campaign-runner --lib`; its
   vacuity guards were NOT weakened — the guarded test path
   (`mock::tests::mock_vmm_composes_maps_memory_and_ticks_per_exit`), the ignored bound
   (≤ 3), and the passed floor (≥ 84) are all rename-invariant and still enforced.
   quality.yml's public-api `-p` list (`campaign-runner`, `sdk-events`) and coverage
   `--ignore-filename-regex` path follow; `.cargo/mutants.toml` globs were already
   path-neutral (comments updated).
4. **Spike branch (PR #98) — checked, not touched.** `git diff main...spike/nested-x86`
   references renamed items in exactly two files, for the foreman's eventual rebase:
   `consonance/vmm-backend/tests/n2_nested_hammer.rs` (new file; imports `Vtime` and
   calls `run_until(Vtime(target))` → becomes `Moment` post-rebase) and `docs/NESTED-X86.md`
   (the spike edits the section containing the "conductor and the live gate harnesses"
   line this sweep reworded → small textual conflict). Nothing was pushed to the spike.
5. **Docs subordinate** — see checklist row above.
6. **History hygiene** — both crate renames are `git mv` (rename detection confirmed in
   the commits).

## Judgment calls a reviewer may want to veto

- **`control_proto::Environment` → `Reproducer`** (2c7b9d9): only `explorer::Environment`
  is named by the table, but the wire struct is the same artifact one layer down (same
  `blob_version` + `bytes` shape, mirrors the explorer blob), and leaving it named
  `Environment` would rebuild the killed collision against the `environment` crate one
  level down. Wire bytes carry no type names; goldens prove identity.
- **`CampaignOracle` renamed to `CrashOracle`, not deleted**: `judge` does delegate
  verbatim to `TerminalOracle`, but the type also carries the proptested
  `is_planted_bug` classification; deletion is a composition change, out of scope for a
  behavior-neutral sweep.
- **`StandingFault::window` audited to `(Moment, Moment)`** (bounds are points). The
  `compose()` fail-closed guard keeps its behavior and its counter-level rationale
  (branch-count window parameterization vs `Moment` offsets; the runtime re-key map
  stays task 93's) — reworded only to stop using the killed type name.
- **`SubjectFactory`'s associated type stays `M`** — the table renames the three trait
  names only; renaming the letter would be improvisation. Doc line updated.
- **`LinkSensor` / `LINK_STATE_CHANNEL` / "link tier" keep**: the table renames the
  crate; the link-tier concept vocabulary is not ruled.

## Gates (all green)

- `cargo build --workspace --all-features` ✓; `cargo nextest run --workspace
  --all-features` ✓ (1663 passed / 28 skipped); guest workspaces: flow-agent 11 ✓
  (+ clippy/fmt), play-agent 53 ✓, payloads build ✓.
- `cargo clippy --workspace --all-features --all-targets -- -D warnings` ✓ on host
  (aarch64-darwin) AND `--target x86_64-unknown-linux-gnu` ✓ (the linux pass also
  compile-checks every `cfg(linux)` box harness this sweep touched).
- `cargo fmt --all -- --check` ✓ · `cargo deny check` ✓ (advisories/bans/licenses/sources).
- Public-api snapshots regenerated for every touched crate; **the snapshot diffs are the
  rename audit** — every changed line is one of the ruled renames, nothing else moved.
  vmm-core and vmm-backend are Linux-frozen snapshots; they were regenerated with
  `cargo +nightly-2026-06-16 public-api --target x86_64-unknown-linux-gnu` (the pinned
  nightly needed `rustup target add x86_64-unknown-linux-gnu`); CI's Linux job verifies
  byte-exactness.
- Full campaign-runner Miri lib suite (the PR #105 nightly command under the new crate
  name: `cargo +nightly-2026-06-16 miri test -p campaign-runner --lib`,
  `MIRIFLAGS=-Zmiri-permissive-provenance`), externally timed with `/usr/bin/time`:
  **84 passed / 0 failed / 3 ignored, rc=0, 707.84 s real (11 min 48 s)** on an
  uncontended M-class Mac — inside the ~12 min budget (task 104 measured 11:46; the
  libtest-printed 1318.89 s is the interpreter's virtual clock, as nightly.yml's comment
  warns). All three nightly guards verified against the log: the
  `mock::tests::mock_vmm_composes_maps_memory_and_ticks_per_exit` line is present and
  `ok`, ignored = 3 (the bound), passed = 84 (the floor).
- No box gate — behavior-neutral by construction (the re-cert window owns the box).

## Known limitations / integrator notes

- The `environment::Moment` ↔ wire-`Moment` name now appears in several crates as the
  deliberate mirror pattern (addendum Pins); where two mirrors meet in one file the wire
  one is alias-imported (`WireMoment`) or fully qualified.
- `docs/EXPLORATION.md` still uses Modulation/Progression as *concept* narration in some
  sections; only code-artifact references (snippets, method names, the settled ruling's
  tense) were updated, per landmine 5's "do not rewrite prose vocabulary beyond that".
- `Cargo.lock` regenerated by the package renames (two name entries; no version moves).
