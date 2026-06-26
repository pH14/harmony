# vtime — implementation notes

Virtual time engine & precise-injection planner per `tasks/05-vtime.md`. All standard
gates and task gates pass on macOS and in a Linux container; see "Gates" below.

## Design in one paragraph

`VClock` stores its config verbatim and computes `vns`/`tsc` straight from the spec's
defining formulas in `u128` intermediates, saturating to `u64::MAX` (one shared
`saturate` helper is the crate-wide overflow rule); `work_for_vns` is the exact ceil
division `ceil(d·den/num)` with its derivation and edge cases documented at the method.
`TimerQueue` keeps a `BTreeMap<(deadline, seq), Entry>` plus a token→key index — the map
order *is* the firing order, so `pop_due` just drains the prefix `<= now`, re-inserting
periodics at `deadline + period` (re-armed entries that are still due fire again in the
same call, which yields drift-free catch-up for free). `InjectionPlanner::stop_at` is a
straight-line implementation of the spec's four cases over `&mut dyn CpuBackend`.
`sim::SimCpu` evolves one xorshift64\* PRNG per instruction for countedness and an
independent one (derived from the same seed) for skid draws in `0..=max_skid`, and logs
every planner-visible interaction (`Armed`/`Stopped`/`Stepped`) for the "assert *how* it
was driven" tests.

## Decisions the integrator should know

- **`VClock::new` rejection rules** ("rejects den == 0 etc."): `ratio_den == 0`,
  `ratio_num == 0` (V-time would never advance, so `work_for_vns` would have no answer
  for any future deadline), and "saturates at trivially small work counts" made precise
  as: `vns(1)` would already exceed `u64::MAX` (i.e.
  `vns_base + floor(num/den) > u64::MAX`). The gate-1 extreme `num = u64::MAX, den = 1,
  vns_base = 0` is *accepted* — `vns(1) == u64::MAX` exactly, `vns(2)` saturates.
  Side effect: a clock whose `vns_base` is within one work-step of `u64::MAX` (e.g.
  restored from an almost-saturated snapshot) is rejected rather than constructed
  pre-saturated.
- **`work_for_vns` on unreachable targets** (target beyond `vns(u64::MAX)`, possible
  when `num < den`): returns `u64::MAX` (saturated ceil division), documented
  best-effort — the round-trip law `vns(work_for_vns(t)) >= t` cannot hold there
  because no `u64` work count satisfies it. Property tests assert the law in the
  reachable regime; a unit test pins the saturating behavior.
- **`tsc` is computed from the saturated `u64` `vns` value**, which is exactly what the
  spec formula `tsc_base + floor(vns(work)·hz/1e9)` says — not from an unsaturated
  internal wide value. Tests assert against an independent recomputation of that
  formula.
- **TimerQueue token semantics**: at most one pending entry per token; re-scheduling an
  already-pending token replaces it (and moves it to the back of its new deadline's
  FIFO class). `cancel` of a periodic removes it entirely. The spec is silent here;
  replace-on-reschedule is the conventional timer semantics and keeps `cancel`
  unambiguous.
- **Periodic re-arm overflow**: if `deadline + period` overflows `u64` (~584 years of
  V-time), the timer is dropped — its next deadline is unrepresentable. Documented at
  `pop_due`; unit-tested.
- **`pop_due` catch-up is unbounded by design**: a periodic popped `n` periods late
  returns `n + 1` firings. With a pathological `(first = 0, period = 1, now = huge)`
  that is a huge vector; it's a pure data structure and the caller controls the
  schedule, so this is documented rather than capped.
- **Planner overshoot during single-stepping** (a backend whose `single_step` advances
  work by more than 1, violating the trait contract) is reported as `SkidExceeded`
  rather than a dedicated variant — it is the same determinism-destroying event
  ("stopped past the target"), with `armed_at` documented as the work count stepping
  started from when nothing was armed. Unreachable with a contract-honoring backend
  (from below the target, +0/+1 steps cannot skip it); covered by a unit test with a
  deliberately broken backend.
- **Planner termination** relies on the backend contract (work monotonic, counted
  events keep occurring). A guest that never retires another counted event would step
  forever — same as real hardware, where that deadline work count is simply never
  reached; documented at `stop_at`.
- **`BackendError`** is an opaque single-field tuple struct (private `String`) with a
  `new(impl Into<String>)` constructor so the future perf_event backend can construct
  it without this crate knowing its failure modes. `VtimeError::Backend` wraps it via
  `#[from]`.
- **`SimCpu` extras beyond the spec'd surface** (additions are allowed; nothing spec'd
  was changed): `SimCpuConfig.initial_work` (property tests start at arbitrary work),
  `instructions_retired()`, and `reset_work_counter()` — the latter models the hardware
  counter restarting at snapshot restore while the instruction stream and skid sequence
  continue, which is what the gate-6 restore scenario needs. `run_until_overflow` with
  an already-passed armed count stops immediately at the current work (a real counter
  armed at a passed count overflows at once); the planner never does this.
- **Sim PRNGs**: xorshift64\* for both the per-instruction countedness draw
  (`next() % den < num`) and the skid draw (`next() % (max_skid + 1)`); the skid stream
  is seeded with `seed ^ 0x9E3779B97F4A7C15` so the two sequences are independent; seed
  0 is mapped to a fixed non-zero constant (xorshift state must be non-zero). Modulo
  bias is irrelevant for test purposes and keeps everything integer-only.
- **Gate 6(b) "event log matches an unsnapshotted reference run"** is tested in two
  layers because `snapshot_vns` quantizes V-time to whole nanoseconds: (1) a clock-level
  property test over *arbitrary* ratios asserts exact vns/tsc equality at the restore
  instant and a proven `<= 1 ns` lag bound thereafter (`floor(a)+floor(b)` vs
  `floor(a+b)`); (2) a full-scenario property test with `ratio_den == 1` (where
  quantization loses nothing) asserts the restored run's complete event log — firings
  and every raw sim interaction, work-rebased — equals the unsnapshotted reference run.
  With fractional ratios the sub-ns remainder is genuinely lost at snapshot time, so
  bit-exact cross-run log equality is not a true property there; the restored run
  itself remains perfectly deterministic. Noted at `snapshot_vns`.
- **Lints**: the crate's `Cargo.toml` does not use `[lints] workspace = true` because
  the task adds `clippy::float_arithmetic = "deny"` and Cargo can't combine workspace
  lints with crate-local ones; the table replicates the workspace's `all = deny` and
  adds the float lint, so it applies to all targets including tests.

## Deviations considered and rejected

- *Allowing `ratio_num == 0`.* A frozen clock makes `work_for_vns` unsatisfiable for
  every future deadline; rejecting at construction is strictly safer and costs nothing.
- *A dedicated error variant for the step-loop overshoot.* The spec's error sketch
  names config errors, `SkidExceeded`, and backend errors; the overshoot is the same
  observable failure as skid (stopped past target) and only reachable from a
  contract-violating backend, so a new variant would be dead weight in every caller's
  match.
- *Batched/closed-form `run_until_overflow` in the sim* (jump work to `armed + skid`
  without per-instruction draws). Faster, but the instruction stream would depend on
  *how* it was driven, breaking the restore test's stream-continuation identity and the
  `instructions_retired` accounting. Per-instruction simulation is fast enough (whole
  suite ≈ 0.6 s).
- *`no_std` core.* Permitted-but-optional in the spec; `TimerQueue` wants
  `alloc::collections::BTreeMap` and `Vec`, and nothing downstream needs `no_std` for
  this crate, so it stays std-only for simplicity.

## Known limitations

- Snapshot restore quantizes V-time at 1 ns (see above): with `ratio_den > 1` a
  restored run can place subsequent injection work targets ±1 counted event relative to
  the hypothetical unsnapshotted continuation. Within any single run (and any replay of
  it) everything remains exactly deterministic.
- `TimerQueue.next_seq` is a plain `u64` increment; 2⁶⁴ insertions are out of scope.
- The planner holds no state across calls (`InjectionPlanner` is just config); callers
  sequence `stop_at` invocations themselves.

## Formal proofs (Kani)

Task quality-f adds `#[cfg(kani)]` bounded-model-checking harnesses (in
`src/clock.rs`, module `proofs`) over `VClock`'s saturating `u128` arithmetic.
These prove "never panics / law holds for ALL inputs in the stated range" —
strictly stronger than the proptest sampling — via CBMC. They compile only under
`cargo kani`; the normal build excludes them, and `Cargo.toml`'s `[lints.rust]
unexpected_cfgs` entry registers the `kani` cfg so the standard clippy gate does
not flag the harnesses. CI runs them in the Linux-only `kani` job
(`.github/workflows/quality.yml`): `cargo install --locked kani-verifier`,
`cargo kani setup`, `cargo kani -p vtime`.

**Where it ran.** Kani is Linux-only and is not installed on the macOS dev host,
so the proofs were run on the project's Linux box (`ssh <det-box>`, Debian 13,
x86-64, Kani 0.67.0 / CBMC). All six harnesses report `VERIFICATION: SUCCESSFUL`
(`Complete - 6 successfully verified harnesses, 0 failures`). CI reruns them on
`ubuntu-latest`.

### Why the bounds (the CBMC cost model)

CBMC bit-blasts arithmetic into a SAT instance; the cost is driven by **operator
width and kind**, largely independent of value-range `assume`s:

- A **symbolic ÷ symbolic** division at `u128` width explodes the instance
  (~0.5M clauses, OOM). The `vns`/`work_for_vns` divisions are `u128`, so their
  divisor (the ratio) must be **concrete** in the arithmetic harnesses to fold
  into a cheap reciprocal-multiply. `VClock::new`'s only division is `ratio_num /
  ratio_den` at `u64` width (64-bit divider), which CBMC discharges even fully
  symbolic — so that harness keeps a wide symbolic ratio.
- A **symbolic × symbolic** multiply is ~quadratic; `tsc`'s `vns * tsc_hz` is
  therefore pinned to a concrete `tsc_hz` (2 GHz), making it symbolic × constant.
- **Exact-equality** assertions across a `u128` divide are far costlier than
  inequalities (the quotient is pinned bit-for-bit), so the two exact harnesses
  (`vns_matches_saturating_spec`, `tsc_no_saturation`) use an aggressive 12-bit
  operand bound, while inequality/clamp harnesses tolerate 24-bit.

Where an operand is bounded, **saturation is still exercised** because the
full-`u64` *base* (`vns_base`/`tsc_base`) — an add operand, not a divide/multiply
operand — drives the running sum across the `u64::MAX` boundary; `saturate()` is
the same code path whichever addend is large.

### Harness catalogue

The fixed proof ratio is `7/3` (a genuine improper fraction exercising both
`floor` rounding in `vns` and `ceil` rounding in `work_for_vns`).

| Harness | Proves | Bound / regime | Time |
|---|---|---|---|
| `new_rejection_rules` | `VClock::new` never panics; applies the den==0 → num==0 → immediate-saturation → accept rules in order | **symbolic** ratio < 2¹⁶ & `tsc_hz` < 2³², `vns_base`/`tsc_base` full `u64` | 129 s |
| `vns_matches_saturating_spec` | `vns` exactly equals `min(vns_base + ⌊work·7/3⌋, u64::MAX)` | ratio 7/3, `vns_base` full `u64`, `work ∈ [0,2¹²)` (exact-eq) | 0.7 s |
| `vns_is_monotone` | `a ≤ b ⇒ vns(a) ≤ vns(b)` | ratio 7/3, `vns_base` full `u64`, `a,b ∈ [0,2²⁴)` | 2.5 s |
| `tsc_no_saturation` | `tsc` exactly equals `tsc_base + ⌊vns(work)·2e9/1e9⌋` when that fits `u64` | ratio 7/3, `tsc_hz`=2 GHz, all operands `∈ [0,2¹²)` (exact-eq) | 15 s |
| `tsc_saturates` | `tsc` clamps to `u64::MAX` whenever the unsaturated sum overflows | ratio 7/3, `tsc_hz`=2 GHz, `tsc_base ∈ {MAX, MAX−1, MAX−2²⁴}` (concrete), `vns_base`,`work ∈ [0,2²⁴)`; `kani::cover!` confirms the saturating regime is reached (non-vacuous) | 1.7 s |
| `round_trip_reaches_target` | round-trip law `vns(work_for_vns(t)) ≥ t` for all bounded targets; non-saturation of `work_for_vns` is **proven** (asserted), not assumed | ratio 7/3, `vns_base`,`t ∈ [0,2²⁴)` | 7.2 s |

### Honest coverage notes

- The arithmetic harnesses prove their laws for the **fixed `7/3` ratio and
  2 GHz `tsc_hz`**, not all ratios — a symbolic `u128` divisor/`tsc_hz` is
  intractable (OOM / >15 min). Wide-range *config-rejection* logic is covered
  separately by `new_rejection_rules` (symbolic ratio up to 2¹⁶).
- The two exact-equality harnesses prove exactness for operands in `[0, 2¹²)`
  (4096 values), which exercises the full multiply/divide/add/truncate rounding
  and carry behavior; larger operands only repeat that arithmetic. Saturation at
  the `u64::MAX` boundary is proven independently (`*_saturates`, and via
  full-`u64` `vns_base` in `vns_matches`).

## Gates

On macOS (Apple Silicon, rustc 1.94.1) and in a Linux container (`rust:1`, aarch64) —
all green:

```
cargo build  -p vtime --all-features
cargo test   -p vtime --all-features      # 53 tests across lib + 5 suites, ≈ 0.6 s
cargo clippy -p vtime --all-features --all-targets -- -D warnings
cargo fmt    -p vtime -- --check
```

Task gates map to: `tests/arithmetic.rs` (gate 1), `tests/planner.rs` (gates 2–3),
`tests/queue.rs` (gate 4), `tests/e2e.rs` (gate 5), `tests/restore.rs` (gate 6), and
the crate-level doc in `src/lib.rs` (gate 7: PMU skid, margin-then-single-step
rationale, real-backend mapping, rr citation). Property tests run 256–512 cases each.
