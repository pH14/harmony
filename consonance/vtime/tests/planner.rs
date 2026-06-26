// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — planner exactness property test (the core gate) — and
//! gate 3 — SkidExceeded is loud when the backend's skid beats the margin.

use proptest::prelude::*;
use std::collections::BTreeSet;
use vtime::sim::{SimCpu, SimCpuConfig, SimEvent};
use vtime::{CpuBackend, InjectionPlanner, PlanOutcome, PlannerConfig, VtimeError};

fn sim(seed: u64, density: (u64, u64), max_skid: u64, initial_work: u64) -> SimCpu {
    SimCpu::new(SimCpuConfig {
        seed,
        density_num: density.0,
        density_den: density.1,
        max_skid,
        initial_work,
    })
    .expect("valid sim config")
}

#[derive(Debug, Clone)]
struct Case {
    seed: u64,
    initial_work: u64,
    skid_margin: u64,
    max_skid: u64,
    distance: u64,
    density: (u64, u64),
}

/// Arbitrary (current work, target > current, max_skid < skid_margin, event
/// density across the full range including sparse streams), with the
/// boundary distances {1, skid_margin, skid_margin + 1} explicitly included
/// and density 1.0 as a degenerate case.
fn planner_case() -> impl Strategy<Value = Case> {
    (1u64..=32).prop_flat_map(|skid_margin| {
        (
            any::<u64>(),
            0u64..1_000_000,
            Just(skid_margin),
            0..skid_margin,
            prop_oneof![
                1 => Just(1u64),
                1 => Just(skid_margin),
                1 => Just(skid_margin + 1),
                3 => 1u64..=400,
            ],
            prop_oneof![
                4 => Just((1u64, 1u64)),                                   // density 1.0
                4 => (1u64..=8, 1u64..=8).prop_map(|(a, b)| (a.min(b), a.max(b))),
                3 => Just((1u64, 16u64)),
                2 => Just((1u64, 100u64)),
                1 => Just((1u64, 1000u64)),                                // sparse
            ],
        )
            .prop_map(
                |(seed, initial_work, skid_margin, max_skid, distance, density)| Case {
                    seed,
                    initial_work,
                    skid_margin,
                    max_skid,
                    distance,
                    density,
                },
            )
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// With max_skid < skid_margin, `stop_at` always stops exactly at the
    /// target; the counted-event distance covered by stepping is bounded by
    /// skid_margin while the instruction count is not; and the reported
    /// `single_steps_used` equals the number of instructions the simulator
    /// actually stepped.
    #[test]
    fn planner_always_stops_exactly(case in planner_case()) {
        let mut cpu = sim(case.seed, case.density, case.max_skid, case.initial_work);
        let planner = InjectionPlanner::new(PlannerConfig { skid_margin: case.skid_margin });
        let target = case.initial_work + case.distance;

        let outcome = planner.stop_at(&mut cpu, target);
        let Ok(PlanOutcome::ReadyToInject { target: t, stopped_at, single_steps_used }) = outcome
        else {
            return Err(TestCaseError::fail(format!("expected ReadyToInject, got {outcome:?}")));
        };

        // Exactness: the loop terminated (we are here) at precisely the target.
        prop_assert_eq!(t, target);
        prop_assert_eq!(stopped_at, target);
        prop_assert_eq!(cpu.work(), target);

        // single_steps_used == instructions the sim actually stepped.
        let stepped: Vec<&SimEvent> = cpu
            .log()
            .iter()
            .filter(|e| matches!(e, SimEvent::Stepped { .. }))
            .collect();
        prop_assert_eq!(single_steps_used, stepped.len() as u64);

        // The counted-event distance covered by stepping is <= skid_margin
        // (single_steps_used itself may be far larger on sparse streams).
        let counted_distance = stepped
            .iter()
            .filter(|e| matches!(e, SimEvent::Stepped { counted: true, .. }))
            .count() as u64;
        prop_assert!(
            counted_distance <= case.skid_margin,
            "stepping covered {} counted events > margin {}",
            counted_distance,
            case.skid_margin
        );

        // How the planner drove the backend, from the sim's event log.
        let arms: Vec<&SimEvent> = cpu
            .log()
            .iter()
            .filter(|e| matches!(e, SimEvent::Armed { .. }))
            .collect();
        if case.distance > case.skid_margin {
            // Far target: exactly one arm, at target - margin; the stop obeys
            // the skid bound and never passes the target.
            prop_assert_eq!(arms.len(), 1);
            prop_assert_eq!(arms[0], &SimEvent::Armed { armed_at: target - case.skid_margin });
            let stop = cpu.log().iter().find_map(|e| match e {
                SimEvent::Stopped { armed_at, skid, stopped_at } => {
                    Some((*armed_at, *skid, *stopped_at))
                }
                _ => None,
            });
            let Some((armed_at, skid, overflow_stop)) = stop else {
                return Err(TestCaseError::fail("no Stopped event after arming"));
            };
            prop_assert_eq!(armed_at, target - case.skid_margin);
            prop_assert!(skid <= case.max_skid);
            prop_assert!(overflow_stop >= armed_at);
            prop_assert!(overflow_stop <= target);
            // And stepping covered exactly the remainder.
            prop_assert_eq!(counted_distance, target - overflow_stop);
        } else {
            // Near target: pure single-stepping, no arming at all.
            prop_assert!(arms.is_empty(), "armed for distance {} <= margin", case.distance);
            prop_assert_eq!(counted_distance, case.distance);
        }
    }
}

/// Gate 2 detail: over a long seeded run the skid draws hit both 0 and
/// max_skid, and every stop is still exact.
#[test]
fn skid_draws_cover_zero_and_max() {
    let max_skid = 3;
    let skid_margin = 8;
    let mut cpu = sim(42, (1, 2), max_skid, 0);
    let planner = InjectionPlanner::new(PlannerConfig { skid_margin });
    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        let target = cpu.work() + skid_margin + 10;
        let outcome = planner
            .stop_at(&mut cpu, target)
            .expect("margin > max_skid cannot fail");
        let PlanOutcome::ReadyToInject { stopped_at, .. } = outcome else {
            panic!("expected ReadyToInject, got {outcome:?}");
        };
        assert_eq!(stopped_at, target);
    }
    for event in cpu.log() {
        if let SimEvent::Stopped { skid, .. } = event {
            seen.insert(*skid);
        }
    }
    assert!(seen.contains(&0), "skid 0 never drawn; draws: {seen:?}");
    assert!(
        seen.contains(&max_skid),
        "max skid never drawn; draws: {seen:?}"
    );
}

/// Gate 2 detail: on a sparse stream (1 counted event per 1000 instructions)
/// the planner terminates, stops exactly, and `single_steps_used` far
/// exceeds the margin while the counted-event distance stays within it.
#[test]
fn sparse_stream_steps_many_instructions_per_event() {
    let skid_margin = 4;
    let mut cpu = sim(0xFEED, (1, 1000), 0, 0); // skid always 0
    let planner = InjectionPlanner::new(PlannerConfig { skid_margin });
    let target = 100;
    let outcome = planner.stop_at(&mut cpu, target).expect("skid 0 < margin");
    let PlanOutcome::ReadyToInject {
        stopped_at,
        single_steps_used,
        ..
    } = outcome
    else {
        panic!("expected ReadyToInject, got {outcome:?}");
    };
    assert_eq!(stopped_at, target);
    // Skid 0 ⇒ the overflow stop is at exactly target - margin, so stepping
    // must cover precisely `skid_margin` counted events...
    let counted = cpu
        .log()
        .iter()
        .filter(|e| matches!(e, SimEvent::Stepped { counted: true, .. }))
        .count() as u64;
    assert_eq!(counted, skid_margin);
    // ...but at 1/1000 density that takes far more than `skid_margin`
    // instructions: the step count is bounded by event density, not skid.
    assert!(
        single_steps_used > 100 * skid_margin,
        "expected ~1000 instructions per counted event, got {single_steps_used} steps total"
    );
}

/// Gate 2 detail: the boundary distances target − now ∈ {1, skid_margin,
/// skid_margin + 1} are exact on every run, deterministically, across the
/// density range (the proptest also draws them, but randomly).
#[test]
fn boundary_distances_are_exact() {
    let skid_margin = 8u64;
    let planner = InjectionPlanner::new(PlannerConfig { skid_margin });
    for density in [(1u64, 1u64), (1, 2), (1, 16), (1, 1000)] {
        for distance in [1u64, skid_margin, skid_margin + 1] {
            for seed in 0..8u64 {
                let initial_work = 1_000;
                let mut cpu = sim(seed, density, skid_margin - 1, initial_work);
                let target = initial_work + distance;
                let outcome = planner
                    .stop_at(&mut cpu, target)
                    .expect("max_skid < margin");
                let PlanOutcome::ReadyToInject { stopped_at, .. } = outcome else {
                    panic!("{density:?}/{distance}/{seed}: expected ReadyToInject");
                };
                assert_eq!(
                    stopped_at, target,
                    "density {density:?} distance {distance}"
                );
                assert_eq!(cpu.work(), target);
                let armed = cpu
                    .log()
                    .iter()
                    .any(|e| matches!(e, SimEvent::Armed { .. }));
                assert_eq!(armed, distance > skid_margin, "arm iff distance > margin");
            }
        }
    }
}

/// Gate 3 — a simulator whose max_skid exceeds the margin eventually
/// produces VtimeError::SkidExceeded, carrying the diagnostic counts.
#[test]
fn skid_exceeding_margin_is_loud() {
    let skid_margin = 4;
    let max_skid = 12; // > margin: overshoot is possible
    let mut cpu = sim(7, (1, 2), max_skid, 0);
    let planner = InjectionPlanner::new(PlannerConfig { skid_margin });

    for attempt in 0..500 {
        let target = cpu.work() + skid_margin + 20;
        match planner.stop_at(&mut cpu, target) {
            Ok(PlanOutcome::ReadyToInject { stopped_at, .. }) => {
                // This attempt drew a small skid; still exact.
                assert_eq!(stopped_at, target);
            }
            Ok(other) => panic!("unexpected outcome {other:?}"),
            Err(VtimeError::SkidExceeded {
                armed_at,
                target: t,
                stopped_at,
            }) => {
                // The diagnostics identify exactly what happened.
                assert_eq!(t, target);
                assert_eq!(armed_at, target - skid_margin);
                assert!(
                    stopped_at > target,
                    "SkidExceeded but {stopped_at} <= {target}"
                );
                assert!(stopped_at <= armed_at + max_skid, "stop beyond max skid");
                // The sim log agrees with the error report.
                let Some(SimEvent::Stopped {
                    armed_at: a,
                    stopped_at: s,
                    ..
                }) = cpu.log().last()
                else {
                    panic!("expected the overshooting Stopped event last");
                };
                assert_eq!((*a, *s), (armed_at, stopped_at));
                return;
            }
            Err(e) => panic!("unexpected error {e:?} on attempt {attempt}"),
        }
    }
    panic!("max_skid {max_skid} > margin {skid_margin} never produced SkidExceeded in 500 tries");
}
