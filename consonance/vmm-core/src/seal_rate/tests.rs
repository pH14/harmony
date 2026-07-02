// SPDX-License-Identifier: AGPL-3.0-or-later
//! Portable unit + property coverage for the seal-rate logic (task 63 gate 1) — the
//! sampling schedule, the bookkeeping, the `sealable` predicate, and the ruling, all
//! against the [`super::mock`] oracle. Runs on macOS and Linux; no `/dev/kvm`.

use super::mock::{MockConfig, MockOracle};
use super::*;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// rate_ppm / formatting
// ---------------------------------------------------------------------------

#[test]
fn rate_ppm_edges() {
    assert_eq!(rate_ppm(0, 0), 0, "empty sweep has no rate");
    assert_eq!(rate_ppm(0, 100), 0);
    assert_eq!(rate_ppm(100, 100), PPM, "all sealed == 100%");
    assert_eq!(rate_ppm(1, 2), 500_000);
    assert_eq!(rate_ppm(1, 3), 333_333, "rounds to nearest ppm");
    assert_eq!(rate_ppm(2, 3), 666_667);
    // Never exceeds 100% even with a pathological numerator.
    assert_eq!(rate_ppm(101, 100), PPM);
}

#[test]
fn ppm_percent_formats() {
    assert_eq!(ppm_percent(PPM), "100.0000%");
    assert_eq!(ppm_percent(0), "0.0000%");
    assert_eq!(ppm_percent(990_000), "99.0000%");
    assert_eq!(ppm_percent(333_333), "33.3333%");
}

// ---------------------------------------------------------------------------
// Sampling schedule
// ---------------------------------------------------------------------------

fn windows() -> Vec<BusyWindow> {
    vec![
        BusyWindow {
            start: 10_000,
            end: 12_000,
            kind: BusyKind::InterruptService,
        },
        BusyWindow {
            start: 50_000,
            end: 51_000,
            kind: BusyKind::WalFsync,
        },
        BusyWindow {
            start: 90_000,
            end: 95_000,
            kind: BusyKind::SchedulerTick,
        },
    ]
}

#[test]
fn build_rejects_degenerate() {
    assert_eq!(
        SamplingSchedule::build(100, 100, 8, &[]),
        Err(ScheduleError::EmptySpan)
    );
    assert_eq!(
        SamplingSchedule::build(0, 100, 0, &[]),
        Err(ScheduleError::ZeroTargets)
    );
    assert!(matches!(
        SamplingSchedule::build(0, 10, 64, &[]),
        Err(ScheduleError::SpanTooNarrow { .. })
    ));
}

#[test]
fn build_n64_split() {
    let ws = windows();
    let s = SamplingSchedule::build(0, 1_000_000, 64, &ws).unwrap();
    assert_eq!(s.len(), 64, "exactly N targets");
    // n/8 == 8, but only 3 windows supplied → 3 busy, 61 uniform.
    assert_eq!(s.busy_count(), 3);
    assert_eq!(s.uniform_count(), 61);
    // Sorted ascending and all in span.
    let ts = s.targets();
    for w in ts.windows(2) {
        assert!(w[0].vtime <= w[1].vtime, "sorted ascending");
    }
    assert!(ts.iter().all(|t| t.vtime < 1_000_000));
    // Each busy target lands inside its window.
    for t in ts {
        if let SampleKind::Busy(k) = t.kind {
            let w = ws.iter().find(|w| w.kind == k).unwrap();
            assert!(
                t.vtime >= w.start && t.vtime < w.end,
                "busy target {} in [{}, {})",
                t.vtime,
                w.start,
                w.end
            );
        }
    }
}

#[test]
fn build_no_windows_all_uniform() {
    let s = SamplingSchedule::build(1_000, 100_000, 64, &[]).unwrap();
    assert_eq!(s.busy_count(), 0);
    assert_eq!(s.uniform_count(), 64);
}

#[test]
fn jitter_zero_is_identity() {
    let s = SamplingSchedule::build(0, 1_000_000, 64, &windows()).unwrap();
    assert_eq!(s.jittered(0), s);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The core schedule invariants hold for any span/n/windows.
    #[test]
    fn prop_schedule_invariants(
        start in 0u64..1_000_000,
        width in 1_000u64..10_000_000,
        n in 1usize..256,
        nw in 0usize..6,
    ) {
        let end = start + width;
        let ws: Vec<BusyWindow> = (0..nw).map(|i| {
            let s = start + (i as u64 + 1) * (width / (nw as u64 + 2));
            BusyWindow { start: s, end: s + (width / 20).max(1), kind: BusyKind::InterruptService }
        }).collect();

        match SamplingSchedule::build(start, end, n, &ws) {
            Ok(sched) => {
                prop_assert_eq!(sched.len(), n);
                prop_assert_eq!(sched.uniform_count() + sched.busy_count(), n);
                let expected_busy = if ws.is_empty() { 0 } else { ws.len().min((n/8).max(1)).min(n) };
                prop_assert_eq!(sched.busy_count(), expected_busy);
                // sorted + in-range
                let ts = sched.targets();
                for w in ts.windows(2) {
                    prop_assert!(w[0].vtime <= w[1].vtime);
                }
                prop_assert!(ts.iter().all(|t| t.vtime >= start && t.vtime < end));
            }
            Err(ScheduleError::SpanTooNarrow { .. }) => {
                prop_assert!((width as u128) < n as u128);
            }
            Err(e) => prop_assert!(false, "unexpected error {e:?}"),
        }
    }

    /// Jitter preserves length, range, and sort order.
    #[test]
    fn prop_jitter_preserves_invariants(
        start in 0u64..100_000,
        width in 10_000u64..1_000_000,
        n in 1usize..200,
        jitter in 0u64..50_000,
    ) {
        let end = start + width;
        let sched = SamplingSchedule::build(start, end, n, &[]).unwrap();
        let j = sched.jittered(jitter);
        prop_assert_eq!(j.len(), sched.len());
        let ts = j.targets();
        for w in ts.windows(2) {
            prop_assert!(w[0].vtime <= w[1].vtime);
        }
        prop_assert!(ts.iter().all(|t| t.vtime >= start && t.vtime < end));
    }
}

// ---------------------------------------------------------------------------
// sealable predicate
// ---------------------------------------------------------------------------

#[test]
fn sealable_truth_table() {
    assert!(sealable(&CpuSnapshot::clean_synchronized()));
    // in-flight injection alone does NOT disqualify (task 41 captures it).
    let mut s = CpuSnapshot::clean_synchronized();
    s.inflight_injection = true;
    s.active_injection = true;
    s.pending_guest_interrupt = true;
    assert!(sealable(&s), "in-flight injection is sealable post-task-41");
    // each of the three real disqualifiers flips it false
    for flip in [
        |s: &mut CpuSnapshot| s.synchronized = false,
        |s: &mut CpuSnapshot| s.rng_mid_exit = true,
        |s: &mut CpuSnapshot| s.unrepresentable = true,
    ] {
        let mut s = CpuSnapshot::clean_synchronized();
        flip(&mut s);
        assert!(!sealable(&s));
    }
}

proptest! {
    /// `sealable` is monotone in the disqualifiers: turning any of them "worse" never
    /// flips false → true.
    #[test]
    fn prop_sealable_monotone(sync in any::<bool>(), rng in any::<bool>(), unrep in any::<bool>()) {
        let base = CpuSnapshot { synchronized: sync, rng_mid_exit: rng, unrepresentable: unrep,
            inflight_injection: false, active_injection: false, pending_guest_interrupt: false };
        // Forcing either disqualifier on (same `synchronized`) ⇒ never sealable.
        let worse = CpuSnapshot { rng_mid_exit: true, unrepresentable: true, ..base };
        prop_assert!(!sealable(&worse));
        // adding non-disqualifier bits never changes the verdict
        let with_inflight = CpuSnapshot { inflight_injection: true, active_injection: true,
            pending_guest_interrupt: true, ..base };
        prop_assert_eq!(sealable(&base), sealable(&with_inflight));
    }
}

// ---------------------------------------------------------------------------
// Bookkeeping over the mock oracle
// ---------------------------------------------------------------------------

fn arb_config() -> impl Strategy<Value = MockConfig> {
    (
        1u64..8_192,   // sync_stride
        0u32..20_000,  // rng_mid_exit_ppm
        0u32..5_000,   // unrepresentable_ppm
        0u32..5_000,   // branch_nondet_ppm
        0u32..600_000, // busy_desync_ppm
        0u32..400_000, // inflight_ppm
        any::<u64>(),  // seed
    )
        .prop_map(
            |(
                sync_stride,
                rng_mid_exit_ppm,
                unrepresentable_ppm,
                branch_nondet_ppm,
                busy_desync_ppm,
                inflight_ppm,
                seed,
            )| {
                MockConfig {
                    sync_stride,
                    rng_mid_exit_ppm,
                    unrepresentable_ppm,
                    branch_nondet_ppm,
                    busy_desync_ppm,
                    inflight_ppm,
                    seed,
                }
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// SealStats is internally consistent for any mock sweep.
    #[test]
    fn prop_sealstats_consistent(cfg in arb_config(), n in 1usize..200) {
        let ws = windows();
        let sched = SamplingSchedule::build(0, 20_000_000, n, &ws).unwrap();
        let oracle = MockOracle::new(cfg, &ws);
        let attempts = oracle.sweep(&sched);
        let stats = SealStats::of(&attempts);
        prop_assert_eq!(stats.n, n);
        prop_assert_eq!(stats.sealed + stats.failed(), n);
        let failsum: usize = stats.by_reason.values().sum();
        prop_assert_eq!(failsum, stats.failed());
        prop_assert_eq!(stats.success_rate_ppm, rate_ppm(stats.sealed, n));
        prop_assert!(stats.success_rate_ppm <= PPM);
    }

    /// Overshoot order statistics are well-formed.
    #[test]
    fn prop_overshoot_ordered(cfg in arb_config(), n in 1usize..200) {
        let sched = SamplingSchedule::build(0, 20_000_000, n, &[]).unwrap();
        let attempts = MockOracle::new(cfg, &[]).sweep(&sched);
        let o = Overshoot::of(&attempts).unwrap();
        prop_assert!(o.min <= o.p50);
        prop_assert!(o.p50 <= o.p90);
        prop_assert!(o.p90 <= o.max);
        prop_assert!(o.mean >= o.min && o.mean <= o.max);
        prop_assert!(o.exact_hits <= n);
        prop_assert_eq!(o.n, n);
    }

    /// Confusion matrix totals to n; precision/recall bounded; perfect when the model has
    /// no unrepresentable and no branch-nondeterministic points (then sealable ⇔ sealed).
    #[test]
    fn prop_predicate_quality(cfg in arb_config(), n in 1usize..200) {
        let ws = windows();
        let sched = SamplingSchedule::build(0, 20_000_000, n, &ws).unwrap();
        let attempts = MockOracle::new(cfg.clone(), &ws).sweep(&sched);
        let q = PredicateQuality::measure(&attempts, sealable);
        prop_assert_eq!(q.true_pos + q.false_pos + q.true_neg + q.false_neg, n);
        prop_assert!(q.precision_ppm <= PPM);
        prop_assert!(q.recall_ppm <= PPM);

        if cfg.unrepresentable_ppm == 0 && cfg.branch_nondet_ppm == 0 {
            // sealable(features) ⇔ Sealed, so no false positives or negatives.
            prop_assert_eq!(q.false_pos, 0, "no precision misses when task 41 holds");
            prop_assert_eq!(q.false_neg, 0, "no recall misses when task 41 holds");
            if q.true_pos > 0 {
                prop_assert_eq!(q.precision_ppm, PPM);
                prop_assert_eq!(q.recall_ppm, PPM);
            }
        }
    }
}

#[test]
fn predicate_quality_hand_built() {
    // Build a sweep with a known confusion matrix.
    let mk = |snap: CpuSnapshot, res: SealResult| SealAttempt {
        target: Target {
            vtime: 0,
            kind: SampleKind::Uniform,
        },
        landed_vtime: 0,
        snapshot: snap,
        result: res,
    };
    let clean = CpuSnapshot::clean_synchronized();
    let desync = CpuSnapshot {
        synchronized: false,
        ..clean
    };
    let attempts = vec![
        mk(clean, SealResult::Sealed), // TP
        mk(
            clean,
            SealResult::Failed(FailureReason::BranchNondeterministic),
        ), // FP (dynamic miss)
        mk(desync, SealResult::Failed(FailureReason::NonSynchronized)), // TN
                                       // a FN cannot occur under the real substrate (a non-sealable landing never seals),
                                       // so recall is exactly TP/(TP) here.
    ];
    let q = PredicateQuality::measure(&attempts, sealable);
    assert_eq!(
        (q.true_pos, q.false_pos, q.true_neg, q.false_neg),
        (1, 1, 1, 0)
    );
    assert_eq!(q.precision_ppm, 500_000);
    assert_eq!(q.recall_ppm, PPM);
}

// ---------------------------------------------------------------------------
// Materialization depth
// ---------------------------------------------------------------------------

#[test]
fn depth_ratio_basic() {
    let d = MaterializationDepth::new(0, 900, 1_000).unwrap();
    assert_eq!(d.from_genesis, 1_000);
    assert_eq!(d.from_parent, 100);
    assert_eq!(
        d.ratio_ppm(),
        100_000,
        "suffix is 10% of the genesis replay"
    );
    assert_eq!(d.savings_ppm(), 900_000);
    assert_eq!(d.ratio_ppm() + d.savings_ppm(), PPM);
}

#[test]
fn depth_rejects_nonmonotonic() {
    assert_eq!(
        MaterializationDepth::new(0, 1_000, 1_000),
        Err(DepthError::NonMonotonic)
    );
    assert_eq!(
        MaterializationDepth::new(500, 400, 1_000),
        Err(DepthError::NonMonotonic)
    );
}

proptest! {
    #[test]
    fn prop_depth_ratio(genesis in 0u64..1_000, gap1 in 1u64..1_000_000, gap2 in 1u64..1_000_000) {
        let parent = genesis + gap1;
        let deep = parent + gap2;
        let d = MaterializationDepth::new(genesis, parent, deep).unwrap();
        prop_assert!(d.from_parent <= d.from_genesis);
        prop_assert!(d.ratio_ppm() <= PPM);
        prop_assert_eq!(d.ratio_ppm() + d.savings_ppm(), PPM);
    }
}

// ---------------------------------------------------------------------------
// The ruling
// ---------------------------------------------------------------------------

fn stats(sealed: usize, n: usize) -> SealStats {
    // A stats value with a chosen rate (reason breakdown irrelevant to the ruling).
    let mut by_reason = BTreeMap::new();
    for r in FailureReason::all() {
        by_reason.insert(r.label(), 0);
    }
    *by_reason.get_mut("non-synchronized").unwrap() = n - sealed;
    SealStats {
        n,
        sealed,
        by_reason,
        success_rate_ppm: rate_ppm(sealed, n),
    }
}

fn overshoot_with_p90(p90: VTime) -> Overshoot {
    Overshoot {
        min: 0,
        max: p90,
        mean: p90 / 2,
        p50: p90 / 2,
        p90,
        exact_hits: 0,
        n: 64,
    }
}

#[test]
fn ruling_go_dense() {
    let inputs = RulingInputs {
        nominal: stats(64, 64),
        adversarial: stats(64, 64),
        determinism_verified: true,
        overshoot: Some(overshoot_with_p90(2_048)),
    };
    assert_eq!(rule(&inputs, RulingThresholds::default()), Ruling::Go);
}

#[test]
fn ruling_grid_when_coarse() {
    let inputs = RulingInputs {
        nominal: stats(64, 64),
        adversarial: stats(63, 64),
        determinism_verified: true,
        overshoot: Some(overshoot_with_p90(5_000_000)), // coarse grid
    };
    assert_eq!(
        rule(&inputs, RulingThresholds::default()),
        Ruling::GoGridRestricted
    );
}

#[test]
fn ruling_nogo_on_low_rate() {
    let inputs = RulingInputs {
        nominal: stats(30, 64), // ~47%
        adversarial: stats(30, 64),
        determinism_verified: true,
        overshoot: Some(overshoot_with_p90(1_000)),
    };
    assert_eq!(
        rule(&inputs, RulingThresholds::default()),
        Ruling::NoGoRestricted
    );
}

#[test]
fn ruling_nogo_on_determinism_gap() {
    let inputs = RulingInputs {
        nominal: stats(64, 64),
        adversarial: stats(64, 64),
        determinism_verified: false, // a seal failed to branch deterministically
        overshoot: Some(overshoot_with_p90(1)),
    };
    assert_eq!(
        rule(&inputs, RulingThresholds::default()),
        Ruling::NoGoRestricted
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Raising the nominal seal rate never worsens the ruling (monotonic gate).
    #[test]
    fn prop_rule_monotone_in_rate(lo in 0usize..64, extra in 0usize..64) {
        let hi = (lo + extra).min(64);
        let th = RulingThresholds::default();
        let dense = Some(overshoot_with_p90(1_000));
        let mk = |sealed| RulingInputs {
            nominal: stats(sealed, 64),
            adversarial: stats(sealed, 64),
            determinism_verified: true,
            overshoot: dense,
        };
        let rank = |r: Ruling| match r {
            Ruling::NoGoRestricted => 0,
            Ruling::GoGridRestricted => 1,
            Ruling::Go => 2,
        };
        prop_assert!(rank(rule(&mk(hi), th)) >= rank(rule(&mk(lo), th)));
    }
}
