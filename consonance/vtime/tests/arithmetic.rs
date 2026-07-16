// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 — arithmetic property tests: monotonicity of `vns`/`tsc`,
//! round-trip laws for `work_for_vns`, and saturation behavior at extreme
//! configs.

use proptest::prelude::*;
use vtime::{VClock, VClockConfig};

const NS_PER_SEC: u128 = 1_000_000_000;

fn saturate(v: u128) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// Independent reference computation of the defining formulas.
fn reference_vns(cfg: &VClockConfig, work: u64) -> u64 {
    saturate(
        u128::from(cfg.vns_base)
            + u128::from(work) * u128::from(cfg.ratio_num) / u128::from(cfg.ratio_den),
    )
}

fn reference_tsc(cfg: &VClockConfig, work: u64) -> u64 {
    saturate(
        u128::from(cfg.guest_base)
            + u128::from(reference_vns(cfg, work)) * u128::from(cfg.guest_hz) / NS_PER_SEC,
    )
}

/// Broad config strategy, biased toward extremes (huge ratios, huge bases),
/// filtered through `VClock::new` so only accepted configs are exercised.
fn any_accepted_config() -> impl Strategy<Value = (VClock, VClockConfig)> {
    let num = prop_oneof![
        4 => 1u64..=1_000_000,
        1 => (u64::MAX - 1_000)..=u64::MAX,
    ];
    let den = prop_oneof![
        4 => 1u64..=1_000_000,
        1 => (u64::MAX - 1_000)..=u64::MAX,
    ];
    let hz = prop_oneof![
        3 => 1u64..=10_000_000_000,
        1 => Just(0u64),
        1 => (u64::MAX - 1_000)..=u64::MAX,
    ];
    let base = prop_oneof![
        3 => 0u64..=1_000_000,
        1 => (u64::MAX - 1_000)..=u64::MAX,
    ];
    (num, den, hz, base.clone(), base).prop_filter_map(
        "config rejected by VClock::new",
        |(ratio_num, ratio_den, guest_hz, guest_base, vns_base)| {
            let cfg = VClockConfig {
                ratio_num,
                ratio_den,
                guest_hz,
                guest_base,
                vns_base,
            };
            VClock::new(cfg).ok().map(|clk| (clk, cfg))
        },
    )
}

/// Moderate config strategy: a regime far from saturation, where the
/// round-trip laws are exact.
fn moderate_config() -> impl Strategy<Value = (VClock, VClockConfig)> {
    (
        1u64..=1 << 20,
        1u64..=1 << 20,
        1u64..=10_000_000_000,
        0u64..=1 << 40,
        0u64..=1 << 40,
    )
        .prop_map(|(ratio_num, ratio_den, guest_hz, guest_base, vns_base)| {
            let cfg = VClockConfig {
                ratio_num,
                ratio_den,
                guest_hz,
                guest_base,
                vns_base,
            };
            (
                VClock::new(cfg).expect("moderate config is always valid"),
                cfg,
            )
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// vns and tsc are monotonic non-decreasing over random increasing work
    /// sequences — including configs that saturate along the way — and always
    /// equal their defining formulas.
    #[test]
    fn vns_tsc_monotonic_and_match_formula(
        (clk, cfg) in any_accepted_config(),
        deltas in proptest::collection::vec(
            prop_oneof![4 => 0u64..=1_000_000, 1 => 0u64..=u64::MAX / 4],
            1..40,
        ),
    ) {
        let mut work = 0u64;
        let mut prev_vns = clk.vns(0);
        let mut prev_tsc = clk.guest_ticks(0);
        prop_assert_eq!(prev_vns, reference_vns(&cfg, 0));
        prop_assert_eq!(prev_tsc, reference_tsc(&cfg, 0));
        for d in deltas {
            work = work.saturating_add(d);
            let v = clk.vns(work);
            let t = clk.guest_ticks(work);
            prop_assert!(v >= prev_vns, "vns decreased: {} -> {} at work {}", prev_vns, v, work);
            prop_assert!(t >= prev_tsc, "tsc decreased: {} -> {} at work {}", prev_tsc, t, work);
            prop_assert_eq!(v, reference_vns(&cfg, work));
            prop_assert_eq!(t, reference_tsc(&cfg, work));
            prev_vns = v;
            prev_tsc = t;
        }
    }

    /// Round-trip law, work side: `work_for_vns(vns(w)) <= w`, with exactness
    /// at the boundary — the result `w'` is the *smallest* work whose vns
    /// reaches `vns(w)`, and lands on it exactly.
    #[test]
    fn roundtrip_from_work((clk, _cfg) in moderate_config(), w in 0u64..=1 << 40) {
        let t = clk.vns(w);
        let w2 = clk.work_for_vns(t);
        prop_assert!(w2 <= w, "work_for_vns(vns({w})) = {w2} > {w}");
        // t is on the clock grid, so the smallest w reaching it hits exactly.
        prop_assert_eq!(clk.vns(w2), t);
        if w2 > 0 {
            prop_assert!(clk.vns(w2 - 1) < t, "w2 = {} not minimal for t = {}", w2, t);
        }
    }

    /// Round-trip law, time side: `vns(work_for_vns(t)) >= t` and the result
    /// is minimal (off-by-one hunting ground).
    #[test]
    fn roundtrip_from_time(
        (clk, t) in moderate_config().prop_flat_map(|(clk, _cfg)| {
            // Keep targets within the reachable range of a 2^40 work budget.
            let hi = clk.vns(1 << 40);
            (Just(clk), 0..=hi)
        }),
    ) {
        let w = clk.work_for_vns(t);
        prop_assert!(clk.vns(w) >= t, "vns(work_for_vns({t})) = {} < {t}", clk.vns(w));
        if w > 0 {
            prop_assert!(clk.vns(w - 1) < t, "w = {} not minimal for t = {}", w, t);
        }
    }
}

// --- Saturation at extreme configs: saturates to u64::MAX, never panics,
// stays monotonic. ---

fn clock(ratio_num: u64, ratio_den: u64, guest_hz: u64, guest_base: u64, vns_base: u64) -> VClock {
    VClock::new(VClockConfig {
        ratio_num,
        ratio_den,
        guest_hz,
        guest_base,
        vns_base,
    })
    .expect("config accepted")
}

#[test]
fn saturation_ratio_one_to_one() {
    // ratio 1/1 with a non-zero base: saturates only at the very top.
    let clk = clock(1, 1, 2_000_000_000, 0, 10);
    assert_eq!(clk.vns(u64::MAX), u64::MAX);
    assert_eq!(clk.vns(u64::MAX - 10), u64::MAX);
    assert_eq!(clk.vns(u64::MAX - 11), u64::MAX - 1);
    // Monotone across the saturation boundary.
    assert!(clk.vns(u64::MAX - 12) <= clk.vns(u64::MAX - 11));
    assert!(clk.vns(u64::MAX - 11) <= clk.vns(u64::MAX - 10));
    assert_eq!(clk.guest_ticks(u64::MAX), u64::MAX);
}

#[test]
fn saturation_huge_num_den_one() {
    // Huge numerator with den 1: accepted (vns(1) == u64::MAX exactly),
    // saturated from work = 2 onward.
    let clk = clock(u64::MAX, 1, 2_000_000_000, 0, 0);
    assert_eq!(clk.vns(0), 0);
    assert_eq!(clk.vns(1), u64::MAX);
    assert_eq!(clk.vns(2), u64::MAX);
    assert_eq!(clk.vns(u64::MAX), u64::MAX);
    let seq: Vec<u64> = [0u64, 1, 2, 3, u64::MAX]
        .iter()
        .map(|&w| clk.vns(w))
        .collect();
    assert!(seq.windows(2).all(|p| p[0] <= p[1]));
}

#[test]
fn saturation_tsc_huge_hz() {
    // tsc saturates while vns is still far from saturating.
    let clk = clock(1, 1, u64::MAX, 0, 0);
    assert_eq!(clk.vns(1 << 40), 1 << 40);
    assert_eq!(clk.guest_ticks(1 << 40), u64::MAX);
    // guest_base alone can saturate the sum.
    let clk = clock(1, 1, 2_000_000_000, u64::MAX - 1, 0);
    assert_eq!(clk.guest_ticks(0), u64::MAX - 1);
    assert_eq!(clk.guest_ticks(1), u64::MAX);
    assert_eq!(clk.guest_ticks(u64::MAX), u64::MAX);
}

#[test]
fn saturation_work_max_everywhere() {
    // work = u64::MAX never panics for a spread of accepted configs.
    for (num, den, hz, tb, vb) in [
        (1u64, 1u64, 2_000_000_000u64, 0u64, 0u64),
        (u64::MAX, 1, u64::MAX, u64::MAX, 0),
        (1, u64::MAX, 1, 0, u64::MAX - 1),
        (7, 3, 1_000_000_000, 5, 1 << 60),
    ] {
        let clk = clock(num, den, hz, tb, vb);
        let v = clk.vns(u64::MAX);
        let t = clk.guest_ticks(u64::MAX);
        assert!(v >= clk.vns(u64::MAX - 1));
        assert!(t >= clk.guest_ticks(u64::MAX - 1));
    }
}
