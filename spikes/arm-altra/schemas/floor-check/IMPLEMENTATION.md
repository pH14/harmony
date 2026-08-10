# IMPLEMENTATION.md — task 141, AA-6 injection attestation (bead hm-oh3v)

Branch `task/aa6-injection-attestation`. Portable, Mac-only; no box run required (the ARM box is
down). Closes the hole where an accidentally-injection-OFF bare matrix passed every AA-6 check:
`check_aa6_matrix` could not tell an injected AA-6 run-set from a bare AA-3 armed-overflow one
(an armed force-exit landing is not an injected interrupt), so a config slip read PASS. This is
the same evidence-integrity class as the PR-98 "structurally unable to fall back to stock and
still pass" lesson.

## What landed

The surface is `spikes/arm-altra/**` only. The change is attestation-scoped:

- **`harness/src/evidence.rs`** — new `InjectionAttestation { enabled, inject_ppi, inject_at_work }`
  (with `on`/`off`/`is_coherent`), a run-set field `RunSet::injection: Option<InjectionAttestation>`,
  a per-record witness `RunRecord::injected: Option<bool>`, and the same field on `RunSetContext`.
  Both new fields are `#[serde(default, skip_serializing_if = "Option::is_none")]` — **optional and
  additive**, so a record/run-set that omits them serializes to byte-identical bytes and every
  retained (pre-attestation) fixture keeps its pinned sha256. SCHEMA_VERSION stays **4** (v4 is the
  offline hardening not yet emitted on the box; this is part of that same hardening).
- **`harness/src/run.rs`** — the per-record witness is set from the actual run: the arm-early exact
  path records `injected: Some(fired)` when injection was configured (`Some(true)` if the PPI was
  asserted at the landing, `Some(false)` on a lost PMI where no landing let it fire); the
  advisory/counting and single-step paths never inject, so they emit `None`.
- **`harness/src/bin/arm_spike.rs`** — `injection_attestation()` stamps the run-set from the config
  the loop **actually executed** (not CLI intent): configured → ON with the PPI; an AA-6 run left
  OFF → stamped `off()` (so the checker fails it with "stamp says OFF", not a missing stamp); other
  non-injecting stages → no stamp. The LinuxGuest `--aa6-record` carries `injected: Some(true)`
  (its emission already refuses a boot where no injection fired), and the `linux-boot` summary line
  now also emits `injection_enabled`/`inject_ppi`/`inject_at_work` — the same stamp the masked lane
  reads.
- **`schemas/floor-check/src/check.rs`** — `check_aa6_matrix` is now injection-aware and
  **fail-closed**: (1) missing stamp ⇒ FAIL; (2) stamp OFF at AA-6 ⇒ FAIL; (3) stamp ON but no
  record's witness is `true` ⇒ FAIL with the counts enumerated (fired / injected=false /
  no-witness); (4) per-class coverage now requires each required class to have a record that both
  **landed** (armed, delivered) **and fired** — the pre-attestation requirement strengthened, never
  weakened. `check_well_formed` refuses an incoherent stamp.
- **`schemas/{run-set,run-record}.schema.json`** — mirror the new fields (`injection` object +
  `injected` boolean), both optional so retained evidence still validates.
- **`schemas/fixtures/` + `fixtures.rs`** — AA-6 fixtures now stamp the ON attestation and carry
  fired witnesses (only the four AA-6 fixtures changed; all non-AA-6 fixtures are byte-identical).
  Three new planted-failure fixtures (the hm-537 negative-control doctrine): `reject-aa6-injection-off`,
  `reject-aa6-missing-attestation`, `reject-aa6-injected-flag-inconsistent`, each asserted in
  `tests/accept_reject.rs` to drive `aa6-matrix` RED for its distinct reason (and the accept gate
  asserts the injection-aware PASS).
- **`host/aa6-masked-digest-{lane.sh,check.py}` + `results/aa-6/masked-digest/RUNBOOK.md`** — the
  masked-digest checker now enforces the harness-stamped `injection_enabled=ON` and the enumerated
  `inject_ppi 22` / `inject_at_work 1` from each rep's summary line (10 checks, was 8), routing it
  and the floor checker through **one** stamped attestation so they cannot disagree about what ran.
  It keeps its existing requirements (mask exactly `{x29, SP}`, injection fired, bit-identity).

## Deviations considered and rejected

- **Bump SCHEMA_VERSION to 5.** Rejected: v4 has not been emitted on the box, so the additive
  fields extend the same not-yet-emitted v4; retained results are v3 and untouched. A bump would
  fail the `schema_version` const cross-check for no benefit.
- **Make `injected`/`injection` required (non-optional).** Rejected: every retained fixture's
  `records.jsonl` is pinned by sha256, so a required field would force re-pinning all 33, and a
  required field is meaningless for non-injecting stages. `Option` + `skip_serializing_if` keeps
  the additive-compat guarantee (proven by a unit test).
- **Derive coverage purely from `injected`, dropping the `overflow.armed && deliveries>=1`
  requirement.** Rejected: the task forbids weakening either checker. Coverage now requires
  **both** the armed-delivered landing and the fired witness.
- **Stamp `enabled` from whether injections actually fired.** Rejected: `enabled` reflects the
  configured posture (the actual config the loop executed); the per-record witnesses are the
  independent fired evidence, and the checker cross-checks the two — a stamp ON with nothing fired
  is exactly the config-slip case gate (3) catches.

## Known limitations / integrator notes

- **Not exercised on silicon.** Like the rest of the checker, the fixtures are model-synthesised;
  the attestation shape is what a real AA-6 run-set will carry, filled with synthetic values. The
  parked hm-3bwm runbook's artifact list is updated for the new stamped fields.
- **`inject_at_work` is lane-specific.** The bare-payload `run` lane injects at every exact landing,
  so it stamps `inject_at_work: None`; only the single-Moment LinuxGuest lane sets it. The checker
  keys ON/OFF on `enabled`, never on `inject_at_work`'s presence.
- **`aa6-merge` needs no change** — it reuses the bare run-set's manifest as its template, so the
  ON stamp and the merged records' witnesses carry through the round-trip unchanged.

## Gates

Green on Mac: `cargo build`, `cargo nextest run` (289 tests), `cargo clippy` native (arm-harness +
floor-check) and `--target aarch64-unknown-linux-gnu` (arm-harness, the KVM/perf `cfg(linux)`
seam), `cargo fmt --check`, `cargo deny check`. The masked-digest Python checker was smoke-tested
against synthetic reps (PASS at 10/10; an injection-OFF batch FAILs `injection-config-on` +
`injection-fired`).
