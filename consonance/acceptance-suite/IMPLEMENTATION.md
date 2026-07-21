# acceptance-suite — implementation notes

The domain layer over `unison`: it turns the generic divergence bisector into
the three determinism oracles (O1–O3 of `docs/DETERMINISM-CORPUS.md`), a corpus
manifest, and a JSON report. Generic over `unison::MachineFactory`, so it is
fully tested today against the toy machine and points at `vmm-core::Vmm<B>` at
integration with no API change.

## Layout

- `src/oracle.rs` — `OracleKind`, `OracleResult`, and `check_determinism` /
  `check_conformance` / `check_seed_sensitivity` (O1/O2/O3).
- `src/manifest.rs` — `CorpusItem`, `CorpusKind`, `ManifestError`,
  `load_manifest` / `to_manifest` / `validate`.
- `src/report.rs` — `ItemReport`, `RunConfig`, and the generic `run_item`
  orchestration (the "caller-supplied factory registry" seam).
- `src/main.rs` — the `acceptance-suite run` / `acceptance-suite validate` binary with the
  task-17 **toy** registry.
- `corpus-manifest.example.toml` — documents the manifest schema (the canonical
  `docs/corpus-manifest.toml` lands with real payloads/goldens in task 18, which
  is outside this task's edit scope).

## The cross-crate edit: `unison::Machine::observable_digest`

O3 is unimplementable on the current `Machine` (it exposed only `state_hash`),
so — as the task authorises — this is the **one** edit outside
`consonance/acceptance-suite/`. `observable_digest()` hashes only the guest's
deliberate output (the toy `out_log`; serial + event capture for the real VMM),
distinct from `state_hash`, which folds in latent state such as the seed-derived
entropy stream. O3 must compare the former: a payload that consumes RNG without
branching on it keeps an identical work count across seeds while its observable
output diverges — but its `state_hash` diverges regardless via the latent PRNG,
so the oracle is unsound on `state_hash`.

**Added as a defaulted trait method (`default = state_hash()`), not a required
one.** This was a forced, deliberate choice: `vmm-core`'s box-only `VmmMachine`
already implements `Machine`, and a *required* method would break that sibling
crate, which conventions rule 1 forbids me from editing. The default is
backward-compatible; `ToyMachine` overrides it (output-log digest) and
`FlakyMachine` delegates to its inner machine. The default is documented as a
fallback that must be overridden for sound O3 — it is never relied on here,
because every machine O3 runs against overrides it. Verified: `vmm-core` and all
its tests still compile against the changed trait.

## OracleKind design

**O1 (`check_determinism`)** — `compare_runs(f, f, seed, …)`; on `Diverged`,
`bisect_divergence` localizes the exact first-divergent work count and it is
attached to the result. A `compare_runs` divergence always reports `passed:
false`; if the bisector cannot reproduce it the result still fails (reason in
`detail`, `divergence: None`) rather than erroring — a detector should not turn a
real finding into an error.

**O2 (`check_conformance`)** — runs once, compares the terminal `state_hash` (hex)
to the golden. A malformed/short/non-hex golden is a `Fail` with a detail, never
a panic (`decode_hex32` is total).

**O3 (`check_seed_sensitivity`)** — runs at two seeds, compares
`observable_digest` (never `state_hash`). `rng_consuming`: assert `work_a ==
work_b` **and** `out_a != out_b`; else assert `out_a == out_b`. Equal seeds are a
`Fail`, not a panic. **Both runs must halt within `limit`** — a run still going at
`limit` makes `work_a == work_b` an artifact of the cap, not of seed-stable
control flow, so a non-terminating payload is reported `inconclusive` (Fail), not
a pass.

## Anti-vacuity of the negative gates (the part most worth reviewing)

- **O1 negative** (`tests/o1_determinism.rs`). `check_determinism` spawns both
  machines from the *same* factory, so a `FlakyFactory` that perturbs every spawn
  identically is vacuous (two identical perturbed machines). The test factory
  `AlternatingFlakyFactory` perturbs **only odd-numbered spawns** (a spawn
  counter; even spawns use the `u64::MAX` "never" sentinel). Because both
  `compare_runs` and `bisect_divergence` spawn machine A before B in fixed
  `(even, odd)` pairs, every comparison pits a clean spawn against a perturbed
  one — so the divergence is **reproducible under the bisector's from-scratch
  re-execution**, and `first_divergent_work` is pinned to the exact perturbation
  work count `T`. (A naive "only the literal 2nd spawn diverges" scheme would not
  survive bisection's many re-spawns.) Persistent `XorPrng` perturbation. ≥256
  cases each for the pass and fail directions.

- **O3 both directions** (`tests/o3_seed_sensitivity.rs`). The two negative
  payloads are built by faithful program transforms over the *real* `ToyMachine`
  (no re-implemented VM): faked determinism = `RAND rd` → `LOADI rd, K` (RAND
  wired to a constant; identical work/control flow, output no longer seed-varying
  → fails RNG-consuming O3); seed leak = a `RAND r5; OUT r5` prefix on a pure
  payload (seed reaches observable output → fails seed-pure O3). Distinct
  **nonzero** seeds are used so the two effective PRNG states differ
  (`spawn(0)` aliases `spawn(ZERO_SEED_STATE)`), making each direction a hard
  guarantee, not a probability. ≥256 cases per direction.

## Mutation testing (PR #48 CI — `cargo mutants --in-diff` is 0 missed)

The new logic is mutation-covered: `cargo mutants --no-shuffle --in-diff` over the
PR diff reports **0 missed** (71 caught, 13 unviable). The CI run first surfaced
ten survivors — test gaps where a line executed but was not constrained — now
killed:

- `registry::stable_hash` (constant-return + `^=`→`|=`/`&=`): pinned to the
  canonical FNV-1a-64 vectors for `""`/`"a"`/`"abc"` plus content/order
  sensitivity (`registry::tests`).
- `check_seed_sensitivity`'s `!halted_a || !halted_b` (`||`→`&&`): a custom
  factory where exactly one seed halts yet both reach `work == limit`
  (`oracle::tests::one_run_not_halting_is_inconclusive_even_when_work_matches`).
- `decode_hex32`'s byte assembly: `(hi << 4) | lo` was rewritten to `(hi << 4) +
  lo`. The nibbles are disjoint, so `|` ≡ `^` there — an **equivalent mutant**
  (the same one `unison` documents); `+` is semantically identical on disjoint
  operands but mutation-distinguishable, killed by the round-trip and
  `decode_hex32_assembles_nibbles_exactly` tests.
- `unison::Machine::observable_digest` default + `FlakyMachine`'s override
  (`→[0;32]`/`[1;32]`): a machine that uses the default, and the flaky override,
  must produce a value equal to the delegate and varying with observed output
  (`unison` `default_observable_digest_is_state_hash_and_varies`,
  `observable_digest_delegates_to_inner_and_varies_with_output`).

## Non-vacuity hardening (PR #48 review — every `all_passed` path is provably non-vacuous)

The cross-model pass found four ways a determinism gate could report green while
testing nothing. All four are closed, each with a test that fails on the old code:

1. **Typo'd manifest key → empty corpus.** `[[items]]` (plural) or `oracle =`
   (singular) silently parsed to an empty/under-specified corpus. Fixed with
   `#[serde(deny_unknown_fields)]` on both manifest DTOs — an unknown key is now a
   hard parse error. (`manifest::tests::deny_unknown_fields_catches_typos`,
   `cli::empty_and_typoed_manifests_fail_loudly`.)
2. **Zero items / unmatched `--item` → vacuous `all_passed: true`.**
   `reports.iter().all(..)` is `true` on an empty slice, so `run --item <typo>`
   (and an empty manifest) exited 0. The binary now errors loudly (exit 1) if the
   manifest is empty or the filter matched nothing; `validate` rejects an empty
   corpus. (`cli::item_filter_typo_fails_loudly`, `validate_rejects_empty_corpus`.)
3. **O3 default seeds collided after normalization.** Default `--seed 0` derived a
   `seed_b` that equalled `unison::toy::ZERO_SEED_STATE`, and `ToyMachine::new(0)`
   normalizes 0 to that same state — so O3 compared two identical effective seeds
   (faked failure for honest RNG payloads, vacuous pass for the seed-leak
   negative). `default_seed_b` now XORs with a constant `∉ {0, ZERO_SEED_STATE}`
   (provably distinct after normalization), and a user-supplied `--seed-b` that
   collides is rejected. (`cli::o3_under_default_seed_distinguishes_rng_from_pure`,
   `colliding_user_seeds_are_rejected`.) The toy registry now lives in the library
   (`acceptance_suite::toy_factory`) so the binary and tests build identical factories.
4. **O3 work-equality vacuous on non-halting runs.** See the O3 halt requirement
   above. (`o3_seed_sensitivity::non_halting_divergence_does_not_pass`.)

Two further same-class corners (PR #48 re-review), closed so the gate is airtight
even under misconfiguration:

5. **An item with `oracles = []` aggregated as green.** Its report has empty
   `results`, and `all([]) == true`. Closed at three layers: `ItemReport::passed()`
   now requires at least one result; `validate` rejects empty-oracle items
   (naming them); and `run` rejects a to-be-run empty-oracle item loudly.
   (`report::tests::item_with_no_oracles_is_never_green`,
   `manifest::tests::validate_rejects_item_with_no_oracles`,
   `cli::item_with_no_oracles_is_rejected`.)
6. **`--limit 0` passed O1 after no work.** `compare_runs` with `limit 0` compares
   zero checkpoints and returns `Identical`. Closed at two layers:
   `check_determinism` reports `inconclusive` (not green) whenever
   `checkpoints_compared == 0`, and the CLI rejects `--limit 0` outright.
   (`oracle::tests::zero_limit_determinism_is_inconclusive_not_green`,
   `cli::limit_zero_is_rejected`.)

## Manifest

TOML, to match `cpu-msr-contract.toml` and the `docs/corpus-manifest.toml` the
design names. Oracles and kinds are stable string tokens (`"determinism"`,
`"seed_sensitivity:rng"`, …) so the file stays human-readable and the parse is
total — an unrecognized token is a documented `Err`, never a panic. Field order
is fixed by the DTO struct layout; no map iteration reaches the bytes (rule 4).
`load_manifest` does structural/token validation only; the semantic
"`Conformance` ⇒ `golden`" rule is `validate` (so the round-trip property holds
for arbitrary structurally-valid item lists). Round-trip is property-tested over
arbitrary item lists (≥256), including rich unicode/punctuation string content.

## Dependency ask-by-comment

`toml` (^0.8) is **outside** the conventions rule-5 whitelist; requested here per
the task's "Default: request `toml`". Rationale: the manifest is a reviewable,
golden-style artifact alongside `docs/cpu-msr-contract.toml`, and the design doc
names it `docs/corpus-manifest.toml`. The declined-fallback was a JSON manifest
parsed with the whitelisted `serde_json`. `cargo deny check` is clean
(advisories/bans/licenses/sources) with the dep and its tree. (Adding this new
crate to the workspace also updates the tracked `Cargo.lock`; that auto-generated
change is committed alongside the crate, matching the precedent of prior
crate-adding task branches — it was not hand-edited.)

## Deviations considered and rejected

- **Required `observable_digest`** — cleaner, but breaks `vmm-core` (see above).
  Rejected; default method chosen.
- **`ManifestError` as a rich enum** (as `unison` did) — the spec sketches `pub
  struct ManifestError(/* via thiserror */)`, so it is an opaque struct whose
  `Display` carries the detail; callers branch on success/failure. Kept to spec.
- **Serde-encoding the `OracleKind` enum directly in TOML** — its mixed unit/struct
  variants make an ugly representation; the string-token DTO is readable and
  round-trips cleanly. Rejected serde-on-the-enum.
- **A custom seed-leak / constant-RNG `Machine`** — the program-transform
  approach reuses the real `ToyMachine`, so the negatives exercise production
  semantics rather than a test double. Rejected the custom machine.

## Known limitations / integrator notes

- **Toy registry only.** `acceptance_suite::toy_factory(source)` maps an item's
  `source` to a `ToyFactory` (decimal `source` = program-generator seed, otherwise
  FNV-hashed); it lives in the library so the binary and tests build identical
  factories. The real-VMM registry is wired at integration by calling the same
  generic `run_item` with a `Vmm<B>` factory — no library change. Goldens are read
  from `item.golden` as a 64-hex file; payload files themselves are task 18.
- **O3 seed pairing is a caller concern.** `check_seed_sensitivity` is generic and
  cannot know a machine's seed normalization, so it only guards raw `seed_a ==
  seed_b`; the *caller* must pass seeds whose effective state differs. The binary
  does this for the toy registry (`effective_toy_seed` / `default_seed_b`); a VMM
  registry must apply the same care for whatever seed→entropy mapping it uses.
- **O4 (backend-equivalence) is intentionally absent.** It needs two real
  backends and is then exactly
  `unison::compare_runs(F_kvm, F_patched, seed, …) == Identical` on a TSC/RNG-free
  payload — expressible later with no new oracle machinery here.
- **No `unsafe`**, so no Miri obligation; nothing added to the `quality.yml` miri
  job.
- **Public-API snapshot** (`tests/public-api.txt`) is frozen; refresh with
  `UPDATE_PUBLIC_API=1 cargo test -p acceptance-suite --test public_api -- --ignored`
  on the pinned nightly after a reviewed API change. The `unison` snapshot was
  likewise refreshed for the new accessor.
