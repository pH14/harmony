// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 5 — end-to-end scenario: VClock + TimerQueue + InjectionPlanner +
//! SimCpu run a 100 µs periodic timer for 1000 firings, including HLT
//! stretches handled by `advance_idle`, with every stop landing exactly on
//! the computed work count and the whole scenario replaying bit-identically.

use vtime::sim::{SimCpu, SimCpuConfig, SimEvent};
use vtime::{
    CpuBackend, InjectionPlanner, PlanOutcome, PlannerConfig, TimerQueue, TimerToken, VClock,
    VClockConfig,
};

const PERIOD_NS: u64 = 100_000; // 100 µs
const FIRINGS: u64 = 1_000;
const TOKEN: TimerToken = TimerToken(7);
/// 1 counted event = 500 ns of V-time (a sparse-branch guest), so each
/// 100 µs period is 200 work units.
const RATIO_NUM: u64 = 500;
const SKID_MARGIN: u64 = 16;
const MAX_SKID: u64 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Firing {
    index: u64,
    deadline_vns: u64,
    target_work: u64,
    stopped_at: u64,
    single_steps_used: u64,
    idle: bool,
    vns_after: u64,
    tsc_after: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScenarioLog {
    firings: Vec<Firing>,
    sim_events: Vec<SimEvent>,
    final_work: u64,
    final_vns: u64,
    instructions_retired: u64,
}

/// The guest "HLTs" before firings 3,4,5 of every block of 11: stretches of
/// consecutive idle deadlines that the loop must warp over with
/// `advance_idle` and fire with zero steps.
fn is_idle_firing(index: u64) -> bool {
    matches!(index % 11, 3..=5)
}

fn run_scenario(seed: u64) -> ScenarioLog {
    let mut clock = VClock::new(VClockConfig {
        ratio_num: RATIO_NUM,
        ratio_den: 1,
        tsc_hz: 2_000_000_000,
        tsc_base: 0,
        vns_base: 0,
    })
    .expect("valid clock config");
    let mut cpu = SimCpu::new(SimCpuConfig {
        seed,
        density_num: 1,
        density_den: 3,
        max_skid: MAX_SKID,
        initial_work: 0,
    })
    .expect("valid sim config");
    let planner = InjectionPlanner::new(PlannerConfig {
        skid_margin: SKID_MARGIN,
    });
    let mut queue = TimerQueue::new();
    queue
        .schedule_periodic(PERIOD_NS, PERIOD_NS, TOKEN)
        .expect("non-zero period");

    let mut firings = Vec::new();
    for index in 0..FIRINGS {
        let (deadline_vns, token) = queue.peek_next().expect("periodic timer always pending");
        assert_eq!(token, TOKEN);

        // HLT stretch: work is frozen, so warp V-time to the deadline.
        let idle = is_idle_firing(index);
        if idle {
            let now_vns = clock.vns(cpu.work());
            assert!(
                deadline_vns >= now_vns,
                "pending deadline cannot be in the past here"
            );
            clock.advance_idle(deadline_vns - now_vns);
        }

        // The loop of the gate: next deadline -> work_for_vns -> stop_at -> pop_due.
        let target_work = clock.work_for_vns(deadline_vns);
        let outcome = planner
            .stop_at(&mut cpu, target_work)
            .expect("margin > max_skid");
        let PlanOutcome::ReadyToInject {
            target,
            stopped_at,
            single_steps_used,
        } = outcome
        else {
            panic!("firing {index}: expected ReadyToInject, got {outcome:?}");
        };

        // Every firing's stop happens at exactly the computed work count.
        assert_eq!(target, target_work);
        assert_eq!(stopped_at, target_work, "firing {index} stopped off-target");
        assert_eq!(cpu.work(), target_work);
        // An idle-warped deadline is already current: fired with zero steps.
        if idle {
            assert_eq!(
                single_steps_used, 0,
                "idle firing {index} required stepping"
            );
        }

        let now_vns = clock.vns(cpu.work());
        assert!(
            now_vns >= deadline_vns,
            "stopped before the deadline was due"
        );
        let fired = queue.pop_due(now_vns);
        assert_eq!(
            fired.as_slice(),
            &[(deadline_vns, TOKEN)],
            "firing {index}: exactly the one due deadline must pop"
        );

        firings.push(Firing {
            index,
            deadline_vns,
            target_work,
            stopped_at,
            single_steps_used,
            idle,
            vns_after: now_vns,
            tsc_after: clock.tsc(cpu.work()),
        });
    }

    ScenarioLog {
        final_work: cpu.work(),
        final_vns: clock.vns(cpu.work()),
        instructions_retired: cpu.instructions_retired(),
        sim_events: cpu.log().to_vec(),
        firings,
    }
}

#[test]
fn thousand_firings_exact_and_replayable() {
    let log = run_scenario(0xDE7E_4515);

    // Sanity on the scenario shape itself.
    assert_eq!(log.firings.len() as u64, FIRINGS);
    for f in &log.firings {
        // The periodic queue has fixed cadence: deadline k is exactly
        // (k+1) * period, and the stop was at the minimal work reaching it.
        assert_eq!(f.deadline_vns, (f.index + 1) * PERIOD_NS);
        assert_eq!(f.stopped_at, f.target_work);
        assert!(f.vns_after >= f.deadline_vns);
        assert!(f.vns_after - f.deadline_vns < RATIO_NUM, "stop not minimal");
    }
    // Idle stretches really happened and really were zero-step.
    let idle_count = log.firings.iter().filter(|f| f.idle).count();
    assert!(idle_count > 200);
    assert!(
        log.firings
            .iter()
            .filter(|f| f.idle)
            .all(|f| f.single_steps_used == 0)
    );
    // Non-idle firings actually exercised the arm-then-step machinery.
    assert!(
        log.firings
            .iter()
            .any(|f| !f.idle && f.single_steps_used > 0)
    );
    let armed = log
        .sim_events
        .iter()
        .filter(|e| matches!(e, SimEvent::Armed { .. }))
        .count();
    assert_eq!(armed, log.firings.iter().filter(|f| !f.idle).count());

    // V-time and TSC are monotone over the firing sequence.
    assert!(
        log.firings
            .windows(2)
            .all(|w| w[0].vns_after <= w[1].vns_after)
    );
    assert!(
        log.firings
            .windows(2)
            .all(|w| w[0].tsc_after <= w[1].tsc_after)
    );

    // Re-running the whole scenario with the same seed reproduces the
    // identical event log, down to every sim interaction.
    let replay = run_scenario(0xDE7E_4515);
    assert_eq!(log, replay);

    // And a different seed produces a different micro-history (the skid and
    // instruction pattern differ), while exactness held throughout anyway.
    let other = run_scenario(0x0BAD_5EED);
    assert_ne!(log.sim_events, other.sim_events);
}
