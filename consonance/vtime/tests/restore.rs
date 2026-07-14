// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 6 — idle-skip and snapshot-restore continuity.
//!
//! (a) `advance_idle` preserves monotonicity and `tsc` keeps matching its
//!     defining formula applied to the new total vns (asserted against the
//!     formula, not as a delta identity — floor makes deltas carry-dependent);
//! (b) snapshot/restore: a fresh `VClock` built from `snapshot_vns` (work
//!     restarting at 0) continues `vns`/`tsc` without discontinuity, and the
//!     continuation's event log matches an unsnapshotted reference run.

use proptest::prelude::*;
use vtime::sim::{SimCpu, SimCpuConfig, SimEvent};
use vtime::{
    CpuBackend, InjectionPlanner, PlanOutcome, PlannerConfig, TimerQueue, TimerToken, VClock,
    VClockConfig,
};

const NS_PER_SEC: u128 = 1_000_000_000;

fn saturate(v: u128) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// The defining tsc formula, recomputed independently from the *observed*
/// vns value (which already includes any idle warps).
fn tsc_formula(guest_base: u64, guest_hz: u64, vns: u64) -> u64 {
    saturate(u128::from(guest_base) + u128::from(vns) * u128::from(guest_hz) / NS_PER_SEC)
}

// --- (a) advance_idle keeps all invariants -------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn advance_idle_keeps_monotonicity_and_tsc_formula(
        ratio_num in 1u64..=1 << 20,
        ratio_den in 1u64..=1 << 20,
        guest_hz in prop_oneof![3 => 1u64..=10_000_000_000, 1 => (u64::MAX - 1_000)..=u64::MAX],
        guest_base in 0u64..=1 << 40,
        vns_base in 0u64..=1 << 40,
        // Interleaved guest work and idle warps; occasional huge warps push
        // vns_base into saturation, which must stay monotone too.
        script in proptest::collection::vec(
            (
                0u64..=1 << 20,
                prop_oneof![5 => 0u64..=1 << 30, 1 => (u64::MAX / 2)..=u64::MAX],
            ),
            1..16,
        ),
    ) {
        let mut clock = VClock::new(VClockConfig {
            ratio_num, ratio_den, guest_hz, guest_base, vns_base,
        }).expect("moderate ratio/base: always accepted");

        let mut work = 0u64;
        let mut prev_vns = clock.vns(work);
        let mut prev_tsc = clock.guest_ticks(work);
        prop_assert_eq!(prev_tsc, tsc_formula(guest_base, guest_hz, prev_vns));

        for (work_delta, idle_delta) in script {
            // Guest runs: work advances.
            work = work.saturating_add(work_delta);
            let mut vns = clock.vns(work);
            let mut tsc = clock.guest_ticks(work);
            prop_assert!(vns >= prev_vns);
            prop_assert!(tsc >= prev_tsc);
            prop_assert_eq!(tsc, tsc_formula(guest_base, guest_hz, vns));
            (prev_vns, prev_tsc) = (vns, tsc);

            // Guest HLTs: V-time warps forward at frozen work.
            clock.advance_idle(idle_delta);
            vns = clock.vns(work);
            tsc = clock.guest_ticks(work);
            prop_assert!(vns >= prev_vns, "advance_idle moved vns backwards");
            prop_assert!(tsc >= prev_tsc, "advance_idle moved tsc backwards");
            // tsc still equals the defining formula applied to the new total
            // vns — it is derived, never tracked separately.
            prop_assert_eq!(tsc, tsc_formula(guest_base, guest_hz, vns));
            // The warp itself is exact (when not saturating): vns advanced by
            // exactly idle_delta at frozen work.
            if vns < u64::MAX && prev_vns.checked_add(idle_delta).is_some() {
                prop_assert_eq!(vns, prev_vns + idle_delta);
            }
            (prev_vns, prev_tsc) = (vns, tsc);
        }
    }

    // --- (b) part 1: clock-level restore continuity, arbitrary ratios ----

    /// Restoring from `snapshot_vns` continues vns/tsc without discontinuity:
    /// exact equality at the snapshot instant; thereafter the restored clock
    /// matches the original to within the 1 ns the snapshot quantized away
    /// (exactly 0 when ratio_den == 1), and stays monotone.
    #[test]
    fn restored_clock_continues_without_discontinuity(
        ratio_num in 1u64..=1 << 20,
        ratio_den in 1u64..=1 << 20,
        guest_hz in 1u64..=10_000_000_000,
        guest_base in 0u64..=1 << 40,
        vns_base in 0u64..=1 << 40,
        snap_work in 0u64..=1 << 30,
        deltas in proptest::collection::vec(0u64..=1 << 24, 1..16),
    ) {
        let cfg = VClockConfig { ratio_num, ratio_den, guest_hz, guest_base, vns_base };
        let original = VClock::new(cfg).expect("moderate config");
        let snap = original.snapshot_vns(snap_work);
        // Work counter restarts at 0; everything else carries over.
        let restored = VClock::new(VClockConfig { vns_base: snap, ..cfg })
            .expect("restored config stays in the accepted regime");

        // No discontinuity at the restore instant.
        prop_assert_eq!(restored.vns(0), original.vns(snap_work));
        prop_assert_eq!(restored.guest_ticks(0), original.guest_ticks(snap_work));

        let mut d = 0u64;
        let mut prev = restored.vns(0);
        for delta in deltas {
            d += delta; // bounded well below overflow by the strategy
            let r_vns = restored.vns(d);
            let o_vns = original.vns(snap_work + d);
            // The restored clock lags by at most the truncated sub-ns
            // remainder: floor(a)+floor(b) <= floor(a+b) <= floor(a)+floor(b)+1.
            prop_assert!(r_vns <= o_vns);
            prop_assert!(o_vns - r_vns <= 1, "restored vns lags by more than 1 ns");
            if ratio_den == 1 {
                prop_assert_eq!(r_vns, o_vns, "integer ratios must restore exactly");
            }
            // tsc inherits the bound through its own floor.
            let r_tsc = restored.guest_ticks(d);
            let o_tsc = original.guest_ticks(snap_work + d);
            prop_assert!(r_tsc <= o_tsc);
            prop_assert!(o_tsc - r_tsc <= guest_hz / 1_000_000_000 + 1);
            // Monotone after restore.
            prop_assert!(r_vns >= prev);
            prev = r_vns;
        }
    }
}

// --- (b) part 2: full-scenario restore, event log vs reference run -------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Firing {
    deadline_vns: u64,
    token: TimerToken,
    /// Work target rebased to the pre-snapshot counter domain, so reference
    /// and restored runs are directly comparable.
    rebased_target: u64,
    single_steps_used: u64,
    vns_after: u64,
}

struct Rig {
    clock: VClock,
    cpu: SimCpu,
    queue: TimerQueue,
    planner: InjectionPlanner,
    /// Work the counter had accumulated before its last restart (0 before a
    /// snapshot restore); rebases logged work values into one domain.
    work_offset: u64,
}

impl Rig {
    fn fire_n(&mut self, n: u64) -> Vec<Firing> {
        let mut firings = Vec::new();
        for _ in 0..n {
            let (deadline_vns, token) = self.queue.peek_next().expect("timer pending");
            let target = self.clock.work_for_vns(deadline_vns);
            let outcome = self
                .planner
                .stop_at(&mut self.cpu, target)
                .expect("skid < margin");
            let PlanOutcome::ReadyToInject {
                stopped_at,
                single_steps_used,
                ..
            } = outcome
            else {
                panic!("expected ReadyToInject, got {outcome:?}");
            };
            assert_eq!(stopped_at, target, "stop must be exact");
            let now_vns = self.clock.vns(self.cpu.work());
            let fired = self.queue.pop_due(now_vns);
            assert_eq!(fired.as_slice(), &[(deadline_vns, token)]);
            firings.push(Firing {
                deadline_vns,
                token,
                rebased_target: self.work_offset + target,
                single_steps_used,
                vns_after: now_vns,
            });
        }
        firings
    }
}

#[derive(Debug, Clone)]
struct RestoreParams {
    seed: u64,
    ratio_num: u64,
    density_den: u64,
    skid_margin: u64,
    max_skid: u64,
    fire_before_snapshot: u64,
    extra_steps: u64,
}

const TOTAL_FIRINGS: u64 = 40;
/// 25 work units per timer period.
const WORK_PER_PERIOD: u64 = 25;

fn restore_params() -> impl Strategy<Value = RestoreParams> {
    (4u64..=12).prop_flat_map(|skid_margin| {
        (
            any::<u64>(),
            50u64..=500,
            1u64..=6,
            Just(skid_margin),
            0..skid_margin,
            1u64..TOTAL_FIRINGS - 1,
            0u64..=10,
        )
            .prop_map(
                |(
                    seed,
                    ratio_num,
                    density_den,
                    skid_margin,
                    max_skid,
                    fire_before_snapshot,
                    extra_steps,
                )| {
                    RestoreParams {
                        seed,
                        ratio_num,
                        density_den,
                        skid_margin,
                        max_skid,
                        fire_before_snapshot,
                        extra_steps,
                    }
                },
            )
    })
}

fn make_rig(p: &RestoreParams) -> Rig {
    let clock = VClock::new(VClockConfig {
        ratio_num: p.ratio_num,
        ratio_den: 1, // integer ratio: snapshot quantization loses nothing
        guest_hz: 2_000_000_000,
        guest_base: 1 << 30,
        vns_base: 0,
    })
    .expect("valid clock");
    let cpu = SimCpu::new(SimCpuConfig {
        seed: p.seed,
        density_num: 1,
        density_den: p.density_den,
        max_skid: p.max_skid,
        initial_work: 0,
    })
    .expect("valid sim");
    let mut queue = TimerQueue::new();
    let period_vns = p.ratio_num * WORK_PER_PERIOD;
    queue
        .schedule_periodic(period_vns, period_vns, TimerToken(1))
        .expect("non-zero period");
    Rig {
        clock,
        cpu,
        queue,
        planner: InjectionPlanner::new(PlannerConfig {
            skid_margin: p.skid_margin,
        }),
        work_offset: 0,
    }
}

/// Rebases the work-count fields of a sim event by `offset`, mapping
/// post-restore events back into the reference run's counter domain.
fn rebase_event(event: &SimEvent, offset: u64) -> SimEvent {
    match *event {
        SimEvent::Armed { armed_at } => SimEvent::Armed {
            armed_at: armed_at + offset,
        },
        SimEvent::Stopped {
            armed_at,
            skid,
            stopped_at,
        } => SimEvent::Stopped {
            armed_at: armed_at + offset,
            skid,
            stopped_at: stopped_at + offset,
        },
        SimEvent::Stepped {
            counted,
            work_after,
        } => SimEvent::Stepped {
            counted,
            work_after: work_after + offset,
        },
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Run a timer scenario to an arbitrary point (k firings plus a few lone
    /// instructions), snapshot, rebuild the clock from `snapshot_vns` with
    /// the work counter restarting at 0, and continue: vns/tsc continue
    /// without discontinuity and the continuation's event log — firings and
    /// every sim interaction — matches the unsnapshotted reference run.
    #[test]
    fn restored_run_matches_unsnapshotted_reference(p in restore_params()) {
        // Reference: no snapshot, straight through. The extra single-steps
        // are mirrored so both runs drive the identical instruction stream.
        let mut reference = make_rig(&p);
        let mut ref_firings = reference.fire_n(p.fire_before_snapshot);
        for _ in 0..p.extra_steps {
            reference.cpu.single_step().expect("sim never fails");
        }
        let ref_events_at_snapshot = reference.cpu.log().len();
        ref_firings.extend(reference.fire_n(TOTAL_FIRINGS - p.fire_before_snapshot));

        // Snapshotted: identical until the snapshot point...
        let mut restored = make_rig(&p);
        let mut rest_firings = restored.fire_n(p.fire_before_snapshot);
        for _ in 0..p.extra_steps {
            restored.cpu.single_step().expect("sim never fails");
        }

        // ...then snapshot the clock and restart the work counter at 0.
        let snap_work = restored.cpu.work();
        let snap_vns = restored.clock.snapshot_vns(snap_work);
        let pre_vns = restored.clock.vns(snap_work);
        let pre_tsc = restored.clock.guest_ticks(snap_work);
        restored.clock = VClock::new(VClockConfig {
            ratio_num: p.ratio_num,
            ratio_den: 1,
            guest_hz: 2_000_000_000,
            guest_base: 1 << 30,
            vns_base: snap_vns,
        }).expect("restored config valid");
        restored.cpu.reset_work_counter();
        restored.work_offset = snap_work;
        let rest_events_at_snapshot = restored.cpu.log().len();

        // No discontinuity across the restore.
        prop_assert_eq!(restored.clock.vns(0), pre_vns);
        prop_assert_eq!(restored.clock.guest_ticks(0), pre_tsc);

        rest_firings.extend(restored.fire_n(TOTAL_FIRINGS - p.fire_before_snapshot));

        // The firing logs match exactly: same deadlines, same tokens, same
        // (rebased) work targets, same step counts, same observed vns.
        prop_assert_eq!(&ref_firings, &rest_firings);

        // And so do the raw sim event logs after the snapshot point, once
        // the restored run's work values are rebased by the snapshot work.
        prop_assert_eq!(ref_events_at_snapshot, rest_events_at_snapshot);
        let ref_tail = &reference.cpu.log()[ref_events_at_snapshot..];
        let rest_tail: Vec<SimEvent> = restored.cpu.log()[rest_events_at_snapshot..]
            .iter()
            .map(|e| rebase_event(e, snap_work))
            .collect();
        prop_assert_eq!(ref_tail, rest_tail.as_slice());
        // (Pre-snapshot prefixes are identical by construction: same seed,
        // same drive.)
        prop_assert_eq!(
            &reference.cpu.log()[..ref_events_at_snapshot],
            &restored.cpu.log()[..rest_events_at_snapshot]
        );
    }
}
