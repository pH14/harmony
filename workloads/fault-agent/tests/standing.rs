// SPDX-License-Identifier: AGPL-3.0-or-later
//! The portable path end to end: real standing-poll response bytes in, the
//! signals and hook launches the agent would apply out.
//!
//! The frames here are built by hand against the documented layout rather than
//! through the shared encoder, so a silent change on either side of the
//! contract fails this test.

use fault_policy::{DecisionClass, Fault, Span, parse_standing, process_target};
use harmony_fault_agent::faults::ActiveFaults;
use harmony_fault_agent::supervisor::{Action, Supervisor};

/// Build a standing-poll response body: `u64 moment`, `u32 count`, then per
/// entry `u16 class`, `u16 target_len`, target bytes, `u64 start`, `u64 end`.
fn answer(moment: u64, entries: &[(u16, Vec<u8>, u64, u64)]) -> Vec<u8> {
    let mut body = moment.to_le_bytes().to_vec();
    body.extend((entries.len() as u32).to_le_bytes());
    for (class, target, start, end) in entries {
        body.extend(class.to_le_bytes());
        body.extend((target.len() as u16).to_le_bytes());
        body.extend(target);
        body.extend(start.to_le_bytes());
        body.extend(end.to_le_bytes());
    }
    body
}

fn process(node: u16, fault: &Fault, window: (u64, u64)) -> (u16, Vec<u8>, u64, u64) {
    (
        DecisionClass::Process.as_u16(),
        process_target(node, fault),
        window.0,
        window.1,
    )
}

fn decode(body: &[u8]) -> ActiveFaults {
    let (_moment, entries) = parse_standing(body).expect("well-formed answer");
    ActiveFaults::from_entries(entries.map(|entry| (entry.class, entry.target, entry.start)))
}

#[test]
fn a_campaign_of_answers_drives_the_expected_signals() {
    let mut supervisor = Supervisor::new(2);

    // Nothing in force: the workload runs untouched.
    let actions = supervisor.tick(&decode(&answer(10, &[])), &[]);
    assert!(actions.is_empty());

    // A hook window and a pause window open together.
    let body = answer(
        20,
        &[
            process(0, &Fault::RunHook(1), (15, 40)),
            process(1, &Fault::ProcPause(Span(20)), (20, 40)),
        ],
    );
    assert_eq!(
        supervisor.tick(&decode(&body), &[]),
        [Action::Stop(1), Action::RunHook(1)]
    );

    // Both windows still contain the Moment: no repeat.
    assert!(supervisor.tick(&decode(&body), &[]).is_empty());

    // Both close and a restart window opens.
    let body = answer(45, &[process(0, &Fault::ProcRestart, (45, 60))]);
    assert_eq!(
        supervisor.tick(&decode(&body), &[]),
        [Action::Kill(0), Action::Cont(1)]
    );

    // The killed node is reaped while the window is open, then brought back
    // when it closes.
    assert!(supervisor.tick(&decode(&body), &[0]).is_empty());
    assert_eq!(
        supervisor.tick(&decode(&answer(60, &[])), &[]),
        [Action::Start(0)]
    );

    let counters = supervisor.counters();
    assert_eq!(counters.ticks, 6);
    assert_eq!(counters.hooks_started, 1);
    assert_eq!(counters.restarts, 1);
    assert_eq!(counters.unexpected_deaths, 0);
    assert_eq!(supervisor.alive_bitmap(), 0b11);
}

#[test]
fn a_hook_window_touching_the_previous_one_launches_the_hook_again() {
    let mut supervisor = Supervisor::new(1);
    let first = answer(20, &[process(0, &Fault::RunHook(1), (15, 40))]);
    assert_eq!(supervisor.tick(&decode(&first), &[]), [Action::RunHook(1)]);
    // The next poll lands inside the following window for the same hook, with
    // no poll having seen the boundary between the two.
    let second = answer(45, &[process(0, &Fault::RunHook(1), (40, 65))]);
    assert_eq!(supervisor.tick(&decode(&second), &[]), [Action::RunHook(1)]);
    assert!(supervisor.tick(&decode(&second), &[]).is_empty());
    assert_eq!(supervisor.counters().hooks_started, 2);
}

#[test]
fn an_empty_answer_decodes_to_no_faults() {
    let active = decode(&answer(0, &[]));
    assert_eq!(active, ActiveFaults::new());
    assert!(active.hooks().is_empty());
}

#[test]
fn a_malformed_answer_is_reported_not_panicked() {
    let good = answer(7, &[process(0, &Fault::ProcKill, (0, 1))]);
    // Truncated at every length, plus a count that outruns the body.
    for len in 0..good.len() {
        assert!(parse_standing(&good[..len]).is_err(), "length {len}");
    }
    let mut lying = good.clone();
    lying[8..12].copy_from_slice(&9_u32.to_le_bytes());
    assert!(parse_standing(&lying).is_err());
    // Trailing bytes past the last entry are refused too.
    let mut extra = good.clone();
    extra.push(0);
    assert!(parse_standing(&extra).is_err());
}
