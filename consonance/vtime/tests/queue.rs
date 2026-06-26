// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — TimerQueue determinism: identical firing sequences on replay,
//! FIFO tie-break for equal deadlines, drift-free periodic re-arm.

use vtime::{TimerQueue, TimerToken};

/// Builds the same queue twice and drives it over the same `pop_due`
/// schedule: the firing sequences must be identical, element for element.
#[test]
fn replay_produces_identical_firing_sequence() {
    let build = || {
        let mut q = TimerQueue::new();
        q.schedule_periodic(1_000, 700, TimerToken(1)).unwrap();
        q.schedule_oneshot(1_000, TimerToken(2));
        q.schedule_periodic(500, 1_300, TimerToken(3)).unwrap();
        q.schedule_oneshot(2_400, TimerToken(4));
        q.schedule_oneshot(99, TimerToken(5));
        q.schedule_periodic(1_000, 700, TimerToken(6)).unwrap();
        q.cancel(TimerToken(4));
        q
    };
    let schedule = [
        0u64, 99, 100, 1_000, 1_001, 2_399, 2_400, 5_000, 5_000, 9_999,
    ];
    let drive = |mut q: TimerQueue| -> Vec<(u64, TimerToken)> {
        let mut all = Vec::new();
        for now in schedule {
            all.extend(q.pop_due(now));
        }
        all
    };

    let a = drive(build());
    let b = drive(build());
    assert_eq!(a, b, "same ops + same schedule must fire identically");
    assert!(!a.is_empty());

    // The output is globally ordered by (deadline, FIFO) within each pop and
    // deadlines never exceed the pop's now: spot-check global monotonicity.
    assert!(a.windows(2).all(|w| w[0].0 <= w[1].0));
}

/// Equal deadlines fire in scheduling (FIFO) order, mixing one-shots and
/// periodics.
#[test]
fn fifo_tie_break_for_equal_deadlines() {
    let mut q = TimerQueue::new();
    q.schedule_oneshot(100, TimerToken(10));
    q.schedule_periodic(100, 50, TimerToken(11)).unwrap();
    q.schedule_oneshot(100, TimerToken(12));
    q.schedule_oneshot(100, TimerToken(13));
    assert_eq!(q.peek_next(), Some((100, TimerToken(10))));
    assert_eq!(
        q.pop_due(100),
        vec![
            (100, TimerToken(10)),
            (100, TimerToken(11)),
            (100, TimerToken(12)),
            (100, TimerToken(13)),
        ]
    );
    // The periodic re-armed at 150.
    assert_eq!(q.peek_next(), Some((150, TimerToken(11))));
}

/// Re-scheduling an existing token replaces its entry and moves it to the
/// back of its new deadline's FIFO class.
#[test]
fn reschedule_moves_to_back_of_fifo_class() {
    let mut q = TimerQueue::new();
    q.schedule_oneshot(100, TimerToken(1));
    q.schedule_oneshot(100, TimerToken(2));
    q.schedule_oneshot(100, TimerToken(1)); // re-schedule: now behind 2
    assert_eq!(
        q.pop_due(100),
        vec![(100, TimerToken(2)), (100, TimerToken(1))]
    );
}

/// Periodic re-arm is fixed-cadence: fire times are exactly first + k·period
/// even when popped late (no drift accumulation), and catch-up firings come
/// out in deterministic deadline order.
#[test]
fn periodic_rearm_has_no_drift() {
    let first = 1_000u64;
    let period = 300u64;
    let mut q = TimerQueue::new();
    q.schedule_periodic(first, period, TimerToken(7)).unwrap();

    // Pop at sloppy, late times; collect every firing's deadline.
    let mut fired = Vec::new();
    for now in [1_299u64, 1_300, 2_905, 2_999, 4_123] {
        fired.extend(q.pop_due(now));
    }
    let expected: Vec<(u64, TimerToken)> = (0u64..=10)
        .map(|k| (first + k * period, TimerToken(7)))
        .collect();
    assert_eq!(
        fired, expected,
        "deadlines must be exactly first + k*period"
    );
}

/// pop_due with now before every deadline pops nothing and peek is stable.
#[test]
fn nothing_due_pops_nothing() {
    let mut q = TimerQueue::new();
    q.schedule_oneshot(500, TimerToken(1));
    assert_eq!(q.pop_due(499), vec![]);
    assert_eq!(q.peek_next(), Some((500, TimerToken(1))));
}
