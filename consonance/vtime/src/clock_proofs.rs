// SPDX-License-Identifier: AGPL-3.0-or-later
//! Kani proof harnesses for `VClock` arithmetic (quality-f), split out of
//! `clock.rs` so cargo-mutants can glob-exclude them: they are `#[cfg(kani)]`
//! and verified by the dedicated `kani` CI job, not the mutation oracle.
//! Declared as `#[cfg(kani)] #[path = "clock_proofs.rs"] mod proofs;` in clock.rs,
//! so it remains a child of `clock` (`use super::*` reaches private items).

use super::*;

// ---------------------------------------------------------------------
// Why two regimes (symbolic-ratio vs concrete-ratio)
//
// `vns`/`work_for_vns` compute `a / b` in **`u128`**. CBMC builds a
// combinational divider whose size is fixed by the *type width* (128 bits),
// not by any value bound on the operands. A 128-bit divider with a
// **symbolic divisor** explodes the SAT instance (~0.5M clauses and growing
// → CBMC runs out of memory), and bounding the operand *values* does not
// shrink the *circuit*. `VClock::new`'s only division, `ratio_num /
// ratio_den`, is `u64 / u64` — a 64-bit divider, which CBMC discharges fine
// even with a fully symbolic divisor — so `new_rejection_rules` keeps a
// wide, symbolic ratio.
//
// The four arithmetic harnesses therefore pin the **ratio to a fixed
// representative fraction** ([`PROOF_NUM`]`/`[`PROOF_DEN`]). With a *concrete*
// divisor CBMC constant-folds `x / 3` into a reciprocal-multiply+shift, which
// is cheap, so each harness can range its remaining symbolic inputs (`work`,
// `vns_base`, `tsc_base`, `tsc_hz`) over a real span instead of OOMing. The
// proven range of each harness is documented on it and summarized in
// `IMPLEMENTATION.md`.

/// Upper bound for the symbolic `ratio_num`/`ratio_den`/`tsc_hz` in
/// `new_rejection_rules`. That harness's only division is the `u64 / u64`
/// `ratio_num / ratio_den` (a 64-bit divider CBMC handles even fully
/// symbolic), so the ratio stays symbolic; 16 bits keeps the instance small
/// while still spanning realistic ratios and reaching `VClock::new`'s
/// `ratio_num == 0`, `ratio_den == 0`, and saturation branches.
const RATIO_BOUND: u64 = (1 << 16) - 1;

/// Fixed work→ns ratio for the arithmetic harnesses: `7 / 3`, a genuine
/// improper fraction that exercises the `floor` rounding in `vns` and the
/// `ceil` rounding in `work_for_vns` (neither is the identity). Concrete so
/// the `u128` divisions constant-fold (see the regime note above).
const PROOF_NUM: u64 = 7;
/// Denominator of the fixed [`PROOF_NUM`]`/`[`PROOF_DEN`] proof ratio.
const PROOF_DEN: u64 = 3;

/// Upper bound for the symbolic `tsc_hz` in `bounded_config`
/// (`new_rejection_rules` only). `VClock::new` does not touch `tsc_hz`, so
/// any bound works; 32 bits (~4 GHz) keeps it realistic.
const TSC_HZ_BOUND: u64 = (1 << 32) - 1;

/// Fixed TSC frequency for `tsc_matches_saturating_spec`: 2 GHz, a realistic
/// virtual-TSC rate. Concrete because in `tsc` the product `vns(work) *
/// tsc_hz` would otherwise be a *symbolic × symbolic* multiply, whose clause
/// count is ~quadratic and OOMs CBMC (unlike the symbolic × *constant*
/// multiplies elsewhere). With `tsc_hz` concrete the product and the
/// `/ NS_PER_SEC` divide both fold to cheap constant operations.
const PROOF_TSC_HZ: u64 = 2_000_000_000;

/// Upper bound (24 bits, ~16.7M) for the symbolic `work`/target arguments
/// that feed a multiplication. Their products with the fixed ratio stay far
/// below the saturation point on their own, so saturation is driven by the
/// **full-`u64`** `vns_base`/`tsc_base` instead (see each harness); the bound
/// caps the multiplicand magnitude and keeps the constant-divisor instances
/// small.
const WORK_BOUND: u64 = (1 << 24) - 1;

/// Aggressive bound (12 bits, 4095) for `tsc_no_saturation` only.
///
/// That harness asserts **exact** equality `tsc == tsc_base + floor(vns*2e9
/// /1e9)`, which pins every bit through the `u128` multiply-*divide*-add.
/// Exact equality across a 128-bit divide is fundamentally costly for CBMC
/// (contrast `round_trip`'s 7 s *inequality* at the 24-bit bound), so this
/// harness uses a much tighter bound. `[0, 2^12)` still drives the full
/// multiply/divide/add/truncate path across thousands of values — all the
/// rounding and carry behavior — and solves in seconds.
const NO_SAT_BOUND: u64 = (1 << 12) - 1;

/// A symbolic [`VClockConfig`] with `ratio_num`/`ratio_den`/`tsc_hz` bounded
/// per [`RATIO_BOUND`]/[`TSC_HZ_BOUND`]; `ratio_den` may be `0` (so
/// `VClock::new`'s rejection of it is reachable). `vns_base`, `tsc_base` are
/// unconstrained `u64`. Used only by `new_rejection_rules`.
fn bounded_config() -> VClockConfig {
    let ratio_num: u64 = kani::any();
    let ratio_den: u64 = kani::any();
    let tsc_hz: u64 = kani::any();
    kani::assume(ratio_num <= RATIO_BOUND);
    kani::assume(ratio_den <= RATIO_BOUND);
    kani::assume(tsc_hz <= TSC_HZ_BOUND);
    VClockConfig {
        ratio_num,
        ratio_den,
        tsc_hz,
        tsc_base: kani::any(),
        vns_base: kani::any(),
    }
}

/// A clock at the fixed [`PROOF_NUM`]`/`[`PROOF_DEN`] ratio with the given
/// `vns_base`, `tsc_hz`, and `tsc_base`. Built by direct construction (not
/// `VClock::new`) so the harness controls every field; the ratio is non-zero
/// by construction.
fn fixed_ratio_clock(vns_base: u64, tsc_hz: u64, tsc_base: u64) -> VClock {
    VClock {
        cfg: VClockConfig {
            ratio_num: PROOF_NUM,
            ratio_den: PROOF_DEN,
            tsc_hz,
            tsc_base,
            vns_base,
        },
    }
}

/// `vns` never panics (no `u128` overflow, no division trap) and equals the
/// saturating spec `min(vns_base + floor(work*7/3), u64::MAX)`.
///
/// Proven for the fixed `7/3` ratio over **all `vns_base ∈ u64`** and
/// `work ∈ [0, 2^12)`. `vns_base` being full-`u64` drives the `saturate()`
/// boundary on both sides. `work` (the only operand fed through the
/// `* 7 / 3` divide) is bounded per [`NO_SAT_BOUND`]: like
/// `tsc_no_saturation` this asserts **exact** equality, and the `/3`
/// reciprocal is itself a full-width 128-bit magic-multiply, so the tight
/// bound keeps it fast.
#[kani::proof]
fn vns_matches_saturating_spec() {
    let vns_base: u64 = kani::any();
    let work: u64 = kani::any();
    kani::assume(work <= NO_SAT_BOUND);
    let clk = fixed_ratio_clock(vns_base, 1, 0);

    let got = clk.vns(work);

    let scaled = u128::from(work) * u128::from(PROOF_NUM) / u128::from(PROOF_DEN);
    let full = u128::from(vns_base) + scaled;
    let want = if full > u128::from(u64::MAX) {
        u64::MAX
    } else {
        full as u64
    };
    assert_eq!(got, want);
}

/// `vns` is monotonic non-decreasing in `work` — the invariant the planner
/// relies on. Proven for the fixed `7/3` ratio over all `vns_base ∈ u64`
/// and `work` operands in `[0, 2^24)` (bounded per [`WORK_BOUND`]).
#[kani::proof]
fn vns_is_monotone() {
    let vns_base: u64 = kani::any();
    let a: u64 = kani::any();
    let b: u64 = kani::any();
    kani::assume(a <= WORK_BOUND && b <= WORK_BOUND);
    kani::assume(a <= b);
    let clk = fixed_ratio_clock(vns_base, 1, 0);
    assert!(clk.vns(a) <= clk.vns(b));
}

// `tsc` is proven in two complementary, each-tractable halves rather than
// one harness. A single harness with `vns_base`/`tsc_base` symbolic over all
// of `u64` inside the `u128` multiply-add (`vns(work)*2e9/1e9 + tsc_base`,
// saturating) is intractable for CBMC (it ran >16 min without finishing) —
// the wide symbolic operands feeding the 128-bit reciprocal-multiply of
// `/ NS_PER_SEC` blow up. Splitting the input space at the saturation
// boundary lets each half bound exactly the operands that matter:
//   * `tsc_no_saturation` — operands bounded so the sum provably stays below
//     `u64::MAX`; asserts `tsc` returns that exact sum.
//   * `tsc_saturates` — operands constrained to the regime where the sum
//     exceeds `u64::MAX`; asserts `tsc` clamps to `u64::MAX`.
// Together they cover the whole spec `min(tsc_base + ticks, u64::MAX)`: the
// non-saturating path is exact and the saturating path clamps. Both keep the
// fixed 2 GHz `tsc_hz` ([`PROOF_TSC_HZ`]) and `7/3` ratio.

/// `tsc` returns the **exact** unsaturated value `tsc_base + floor(vns(work)
/// * 2e9 / 1e9)` whenever that value fits in `u64`.
///
/// Proven for the fixed `7/3` ratio and 2 GHz `tsc_hz` over
/// `vns_base, tsc_base, work ∈ [0, 2^12)` ([`NO_SAT_BOUND`]) — bounds that
/// make the sum provably `< u64::MAX` (so neither `vns` nor `tsc`
/// saturates), exercising the exact-arithmetic path. The tight bound is
/// required because exact equality across the `u128` divide is far costlier
/// than `round_trip`'s inequality (see [`NO_SAT_BOUND`]).
#[kani::proof]
fn tsc_no_saturation() {
    let vns_base: u64 = kani::any();
    let tsc_base: u64 = kani::any();
    let work: u64 = kani::any();
    kani::assume(vns_base <= NO_SAT_BOUND);
    kani::assume(tsc_base <= NO_SAT_BOUND);
    kani::assume(work <= NO_SAT_BOUND);
    let clk = fixed_ratio_clock(vns_base, PROOF_TSC_HZ, tsc_base);

    let vns = clk.vns(work);
    let ticks = u128::from(vns) * u128::from(PROOF_TSC_HZ) / NS_PER_SEC;
    let full = u128::from(tsc_base) + ticks;
    // The bounds above keep `full` far below `u64::MAX`, so `tsc` is exact.
    assert!(full < u128::from(u64::MAX));
    assert_eq!(clk.tsc(work), full as u64);
}

/// `tsc` **clamps to `u64::MAX`** in the saturating regime — for the concrete
/// `tsc_base` values this harness drives, every `vns_base`/`work` whose
/// unsaturated sum exceeds `u64::MAX` returns `u64::MAX`.
///
/// The clamp is *value-independent* by design — `tsc` returns `u64::MAX`
/// however far the sum overflows — so concrete `tsc_base` values that trigger
/// saturation are representative; a symbolic high-magnitude `tsc_base` in the
/// `u128` multiply-add only slows CBMC. This harness iterates `tsc_base` over
/// `{u64::MAX, u64::MAX - 1, u64::MAX - 2^24}` with `vns_base, work ∈
/// [0, 2^24)` (`work >= 1`), at the fixed `7/3` ratio and 2 GHz `tsc_hz`.
///
/// SCOPE: this is *concrete* coverage of the saturating side, not a symbolic
/// proof over all `tsc_base`. A `tsc_base`-specific defect at a value outside
/// the set above (e.g. a stray special case) is not in scope here; the exact
/// non-saturating path is covered by `tsc_no_saturation`. The `kani::cover!`
/// confirms the saturating regime is actually reached, so the guarded
/// assertion is not vacuous.
#[kani::proof]
#[kani::unwind(4)]
fn tsc_saturates() {
    let vns_base: u64 = kani::any();
    let work: u64 = kani::any();
    kani::assume(vns_base <= WORK_BOUND);
    kani::assume(work >= 1 && work <= WORK_BOUND);

    for &tsc_base in &[u64::MAX, u64::MAX - 1, u64::MAX - WORK_BOUND] {
        let clk = fixed_ratio_clock(vns_base, PROOF_TSC_HZ, tsc_base);
        let vns = clk.vns(work);
        let ticks = u128::from(vns) * u128::from(PROOF_TSC_HZ) / NS_PER_SEC;
        let sum = u128::from(tsc_base) + ticks;
        kani::cover!(sum > u128::from(u64::MAX), "saturating regime reachable");
        if sum > u128::from(u64::MAX) {
            assert_eq!(clk.tsc(work), u64::MAX);
        }
    }
}

/// Round-trip law: `vns(work_for_vns(t)) >= t` for every bounded target.
///
/// `work_for_vns` returns `u64::MAX` best-effort only for an *unreachable*
/// target (one that no work count satisfies). For the bounded inputs here
/// (`t, vns_base ∈ [0, 2^24)`) every target IS reachable, so we **assert**
/// `work_for_vns(t) != u64::MAX` rather than assume it — turning
/// non-saturation into a proof obligation. (Assuming `w < u64::MAX` on the
/// function's own output would silently discard exactly the cases a
/// regression — e.g. `work_for_vns` wrongly returning `u64::MAX` for a
/// reachable target — would break, defeating the proof.)
///
/// Proven for the fixed `7/3` ratio over `vns_base, t ∈ [0, 2^24)`. The law
/// is a structural property of the chained `ceil`/`floor` rounding, fully
/// exercised across this 16.7M-wide span of targets; the bound caps
/// `d = t - vns_base`, the dividend feeding `work_for_vns`'s division.
#[kani::proof]
fn round_trip_reaches_target() {
    let vns_base: u64 = kani::any();
    let t: u64 = kani::any();
    kani::assume(vns_base <= WORK_BOUND);
    kani::assume(t <= WORK_BOUND);
    let clk = fixed_ratio_clock(vns_base, 1, 0);

    let w = clk.work_for_vns(t);
    // A bounded target is always reachable, so work_for_vns must not saturate.
    // Proving this (vs assuming it) is what lets the law catch a work_for_vns
    // regression that returns u64::MAX for a reachable target.
    assert!(
        w < u64::MAX,
        "work_for_vns saturated for a bounded, reachable target"
    );
    assert!(clk.vns(w) >= t);
}

/// `VClock::new` never panics and applies its rejection rules exactly, in
/// the documented priority order, for every (bounded) config: zero
/// denominator, then zero numerator, then immediate (`work == 1`)
/// saturation; otherwise it accepts.
#[kani::proof]
fn new_rejection_rules() {
    let cfg = bounded_config();
    let result = VClock::new(cfg);

    if cfg.ratio_den == 0 {
        assert_eq!(result, Err(VtimeError::ZeroRatioDen));
    } else if cfg.ratio_num == 0 {
        assert_eq!(result, Err(VtimeError::ZeroRatioNum));
    } else {
        let step_vns = cfg.ratio_num / cfg.ratio_den;
        let saturates = u128::from(cfg.vns_base) + u128::from(step_vns) > u128::from(u64::MAX);
        if saturates {
            assert_eq!(
                result,
                Err(VtimeError::ImmediateSaturation {
                    vns_base: cfg.vns_base,
                    step_vns,
                })
            );
        } else {
            assert!(result.is_ok());
        }
    }
}
