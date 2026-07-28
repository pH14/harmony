# W4 — ARM spike harness input + stage-gating hardening (hm-kdih)

One pass over the `arm-spike` / `arm-el0-count` argument, stage and schema validation,
closing the five PR #132 findings `hm-ej5`, `hm-usj`, `hm-8z7`, `hm-bec`, `hm-fou`. All
five are operator-facing (none is reachable by an untrusted guest); the value is that the
least-defended code in an evidence-integrity project stops being able to (a) mislabel what
it measured or (b) hang/OOM a box window silently.

Surface touched: `harness/src/bin/arm_spike.rs`, `harness/src/el0.rs`,
`harness/src/run.rs`, `harness/src/bin/arm_el0_count.rs`, and
`schemas/run-set.schema.json`. No other files.

---

## hm-ej5 (J15) — exact-landing path gated to the stages that certify it

**The evidence question first, because a reviewer will ask it first: was a mislabelled
AA-1 manifest ever actually producible? No.** Every retained `stage: "aa1"` run-set carries
`skid_margin: null`, `expected_exit_reason: "signal-kick"`, `kvm_patched: false`:

```
results/aa-1c/aa1c-armed-smoke-001/run-set.json   skid=null exit=signal-kick patched=false
results/aa-1b/aa1b-pinned-solo-001/run-set.json   skid=null exit=signal-kick patched=false
results/aa-1b/aa1b-smoke-001/run-set.json         skid=null exit=signal-kick patched=false
```

The mislabelled artifact (an AA-1 manifest with an exact `skid == 0` landing on the patched
`Preempt`) was never emitted — all AA-1 evidence rode the stock signal-kick, as AA-1's
calibration is supposed to. So this is a **code fix, not an evidence problem**; nothing
retained has to be re-adjudicated.

**The bug.** `run_sample_exact` was selected on `Preempt + skid_margin` **alone**, at any
stage. So `--stage aa1 --mechanism patched --skid-margin N` would have recorded an exact
`skid == 0` landing under an AA-1 label — false evidence of the very skid AA-1 exists to
*measure*.

**The fix.** A portable, unit-tested `select_sample_path(stage, mechanism, skid_margin)`
decides the counting-mode path once, up front, and **refuses the exact combination outright**
at any stage that does not certify the exact landing. The certifying set is
`{Aa3, Aa4, Aa6}` — the stages whose acceptance rides the patched force-exit mechanism,
i.e. exactly the floor checker's own `requires_patched_mechanism` set (AA-3 is the exact-
landing contract; AA-4 injects through AA-3's machinery; AA-6 is the mini determinism gate +
injection at exact landed Moments). I deliberately did **not** hard-code "require aa3": the
retained `stage: "aa6"` mini-gate run-sets (`skid=53`, `preempt`, 6000 records) are a real,
legitimate `arm-spike run` exact-path flow, and gating to aa3 only would have broken re-
running them. The decision is a preflight (before the VM is built or a sample measured), so
a rejected combination never writes a partial, mislabelling manifest.

Patched **without** a margin still takes the arm-at-target reliability proxy (`run_sample`),
and the stock kick is never the exact path — both unchanged.

Regression: `the_exact_landing_path_is_gated_to_the_stages_that_certify_it` (in
`arm_spike.rs`) asserts AA-1/AA-0/AA-2/AA-5 + patched + margin are refused by name, and
AA-3/AA-4/AA-6 select `Exact { margin }`.

---

## hm-usj (J17) — `kvm_mode: protected` accepted by the run-set schema

`sys::kvm_mode()` reads the effective mode from the kernel's own boot line and returns one
of `vhe` / `nvhe` / `protected` (the last is protected nVHE / pKVM, a **supported** mode).
The `run` loop's live cross-check compares that read against `environment.kvm_mode` and
**passes `protected` through** — but the run-set schema's `kvm_mode` enum permitted only
`vhe`/`nvhe`, so a faithfully-recorded protected-mode run wrote a schema-**invalid**
manifest. Silently writing an invalid manifest on a supported mode is the worst option.

**The fix, chosen over "reject before writing":** add `protected` to the schema enum (the
schema is in the surface list; `gen-run-inputs.py` is not, and rejecting a *supported* mode
the harness itself reads and cross-checks would be discarding real machine state). The Rust
`Environment.kvm_mode` is a free `String` and the floor checker only enforces `minLength 1`
on it, so no other change is needed. Backward-compatible: every committed fixture (all
`vhe`) still validates — floor-check's `every_fixture_conforms_to_the_committed_json_schemas`
stays green.

Regression: `the_run_set_schema_accepts_every_kvm_mode_the_harness_can_record` (in
`arm_spike.rs`) parses the committed schema and asserts its enum covers every value
`sys::kvm_mode` can record. Fails before (enum = `[vhe, nvhe]`), passes after.

---

## hm-8z7 (J14) — EL0 plan product / record / file ceilings

`el0_plan` had no preflight bound, so `arm-el0-count --cases u64::MAX` OOM-killed the tool.
This mirrors the guest planner (`plan::plan`), which already bounds
`cells × cases × reps` against `MAX_PLANNED_SAMPLES`.

`el0_plan_bounded` is the new guarded entry point the binary calls (the pure `el0_plan`
stays infallible for the deterministic core and tests). It checks three **named** ceilings,
in order, and refuses **before allocating anything**:

1. **Product ceiling** — `classes × scales × cases × reps` via `checked_mul`; a near-
   `u64::MAX` argument that would wrap to a small count is `El0PlanError::ProductOverflow`.
2. **Record ceiling** — a large-but-finite count over `MAX_EL0_RECORDS` (10M, matching the
   sibling) is `RecordCeiling { records }`.
3. **File ceiling** — a count under the record ceiling whose estimated JSONL
   (`records × EL0_RECORD_BYTES_ESTIMATE`) exceeds `MAX_EL0_FILE_BYTES` (1 GiB) is
   `FileCeiling { records, bytes }` — an independent guard so a future record-shape change
   cannot re-introduce a multi-gigabyte write.

Every variant names the ceiling and the offending value. Regressions (in `el0.rs`) exercise
each of the three by name and confirm a realistic sweep plans unchanged.

Overlap with **hm-7np** (W2): hm-7np's scan-bound sub-item (`aa4-exclusive-scan.py:60`
unbounded `read_bytes`) is the *same species* of operator-run DoS but a different file
(Python, outside this surface). No shared code was fixed twice; a note was left on hm-7np so
W2 does not treat this bead as having covered its scan-bound item.

---

## hm-bec (J16) — post-landing duplicate-delivery storm is capped

`run_sample` already bounded the *below-target advisory* path (`MAX_ADVISORY_EXITS`), but
the **post-landing** `deliveries` path (both `run_sample` and `run_sample_exact`) was
unbounded: after the real landing, the patched vCPU can keep force-exiting on host IRQs at/
above the target, each a duplicate delivery. Each is a fresh `KVM_RUN`, which resets the
per-`KVM_RUN` watchdog — so the watchdog never trips and the sample **hangs** instead of
failing. A hang is worse than a failure here: it burns a box window silently.

**The fix.** `MAX_DELIVERIES` (100_000, generous — a clean run delivers exactly once, and
the multiplicity check already flags any duplicate) caps the post-landing deliveries in both
loops; exceeding it is `RunError::DeliveryStorm { deliveries, landed }`.

Regressions (in `run.rs`): `a_post_landing_delivery_storm_is_refused_not_spun_on`
(`run_sample`) and `the_exact_path_also_caps_a_post_landing_delivery_storm`
(`run_sample_exact`) script `MAX_DELIVERIES + 1` post-landing Preempts and assert
`DeliveryStorm`. Both **fail before / pass after**: pre-fix the loop consumes the whole
storm and never returns `DeliveryStorm` (in the field it hangs; against the scripted vCPU it
runs the storm out to a different result); post-fix it stops at the cap. Marked
`#[cfg_attr(miri, ignore)]` like the existing advisory-storm test (100k iterations of a `>`
check, no unsafe).

---

## hm-fou (J13) — a portable, Miri-exercisable seam for the EL0 bookkeeping

**Be honest about what this does and does not buy.** The genuinely unsafe measurement
surface in `arm_el0_count.rs` — the raw `rt_sigaction` / `mmap` / `ucontext` syscalls, the
`global_asm!` window bodies, and the `perf_event` counter — **remains outside Miri's reach,
and that is inherent**: Miri executes no aarch64 machine code and issues no syscalls, and
that whole `measure` module is `#[cfg(all(target_os = "linux", target_arch = "aarch64"))]`,
so it is compiled out on every Miri host. Nothing here changes that.

What *was* factored out and brought under Miri is the **logic around it**: the record-
assembly and first-failure bookkeeping loop that used to live inside that cfg-gated module
(so Miri never saw it either). It now lives in `el0.rs` as `collect_el0_records`, a portable
function parameterized by a measurement seam (`FnMut(usize, &El0Sample) -> Result<
El0Measurement, String>`). The binary plugs the real (unsafe) counter loop into the seam; a
**loopback fake** drives the same function — per-class trip derivation, dense sample-id
assignment, class/scale naming, stop-at-first-failure keeping partial evidence — under Miri.

So the accurate summary is **not** "Miri now covers `arm_el0_count`". It is: the EL0
*bookkeeping* is now under Miri (`collect_el0_records`, plus the three plan-ceiling checks
and `assemble_el0_set`, all pure); the syscall/asm/perf measurement is not and cannot be,
and is exercised only by the on-silicon runs under `results/`. A vacuous "Miri covers it
all" claim would itself be a finding, and the Gate Auditor seat hunts exactly that.

Regressions (in `el0.rs`, Miri-run): `collect_assembles_one_record_per_sample_with_
derived_trips` and `collect_stops_at_the_first_failure_but_keeps_the_partial_evidence`.
`arm-harness` is already in the `quality.yml` `miri` job, so no CI change was needed.

---

## Gates

Run for `arm-harness` (native aarch64-macOS **and** the `aarch64-unknown-linux-gnu` box
target, which is what compiles the `cfg(aarch64-linux)` `measure` module) plus the schema
consumers:

- `cargo nextest run -p arm-harness --all-features` — 177 pass (+10 new).
- `cargo check --target aarch64-unknown-linux-gnu -p arm-harness --all-features --bins` — clean.
- `cargo clippy -p arm-harness --all-features --all-targets -- -D warnings` — native + box target clean.
- `cargo fmt -p arm-harness -- --check` — clean.
- `cargo +nightly-2026-06-16 miri test -p arm-harness` with `MIRIFLAGS=-Zmiri-permissive-provenance` — the new EL0 seam runs under the interpreter.
- `cargo nextest run` on `floor-check` — 133 pass; the schema change breaks no fixture.
- `cargo deny check` — advisories/bans/licenses/sources ok. **No new dependencies** were
  added (`thiserror` and `serde_json` were already `arm-harness` deps).

No existing check was weakened to make a bound fit.

## Notes for the integrator

- The two evidence-integrity items (hm-ej5, hm-usj) are the ones to scrutinize; the other
  three are robustness. hm-ej5's evidence question is answered above (no producible
  mislabelled AA-1 manifest), so the merge does not depend on a re-adjudication.
- Leave a note on **hm-7np** that this bead handled the EL0 plan bound but NOT the
  `aa4-exclusive-scan.py` scan-bound sub-item (different file, outside this surface).
