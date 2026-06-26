// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 35 — exact-value tests that kill the mutants the first full-tree
//! `cargo mutants` run left surviving (or only timeout-caught) in this crate.
//!
//! These are *test-tightness* gaps: the production logic is correct, but the
//! existing suite did not pin the exact boundary/length/value a mutated operator
//! or loop bound would change. Each test below asserts the precise value either
//! side of a boundary, so flipping the operator (or the loop bound / accumulator)
//! makes the test fail rather than silently pass — or, for the loop bounds,
//! fail *fast by assertion* on a small input instead of only by the ~372 s hang.

use environment::{
    Answer, DecisionPoint as P, EnvError, Environment, FaultPolicy, MAX_SUPPLY_LEN, Outcome,
    SeededEnv,
};

/// Pull the `Supply` bytes a `SeededEnv` gives for an `Entropy { bytes: n }`
/// decision.
fn entropy_supply(env: &mut SeededEnv, n: u32) -> Vec<u8> {
    match env.decide(&P::Entropy { bytes: n }) {
        Outcome::Resolved(Answer::Supply(v)) => v,
        other => panic!("entropy must Supply, got {other:?}"),
    }
}

/// `catalog.rs:178` `DecisionPoint::admits` — the scheduler-selection bound is
/// `selection < ready` (strict). A `<`→`<=` mutant would wrongly admit a
/// selection *equal to* `ready` (an out-of-range index `0..ready`). Pin the exact
/// boundary: `ready-1` admissible, `ready` and `ready+1` not.
#[test]
fn scheduler_selection_bound_is_strict() {
    let p = P::Scheduler { ready: 5 };
    let supply = |sel: u32| Answer::Supply(sel.to_le_bytes().to_vec());

    assert!(p.admits(&supply(0)), "0 < 5 is admissible");
    assert!(p.admits(&supply(4)), "ready-1 (4 < 5) is admissible");
    assert!(
        !p.admits(&supply(5)),
        "a selection equal to ready (5) is out of range 0..5 and inadmissible"
    );
    assert!(!p.admits(&supply(6)), "6 > 5 is inadmissible");

    // The same boundary observed through `RecordedEnv`: a `ready`-valued override
    // is ignored, so the seeded base answers; a `ready-1` override wins.
    let at_bound = supply(5);
    let mut env = recorded_with_override(7, 0, at_bound);
    let mut base = SeededEnv::new(7, FaultPolicy::none());
    assert_eq!(
        env.decide(&p),
        base.decide(&p),
        "override selecting exactly `ready` is ignored; base answers"
    );

    let in_range = supply(4);
    let mut env2 = recorded_with_override(7, 0, in_range.clone());
    assert_eq!(
        env2.decide(&p),
        Outcome::Resolved(in_range),
        "an in-range override (4 < 5) wins"
    );
}

/// A `RecordedEnv` whose only override is `ans` at decision `id`.
fn recorded_with_override(seed: u64, id: u64, ans: Answer) -> environment::RecordedEnv {
    environment::EnvSpec::Recorded {
        seed,
        policy: FaultPolicy::none(),
        overrides: vec![(environment::DecisionId(id), ans)],
        standing: vec![],
    }
    .materialize()
}

/// `codec.rs:141` `read_answer` — a decoded `Supply` is rejected only when its
/// length is `> MAX_SUPPLY_LEN`. A `>`→`==` mutant would reject *only* the exact
/// max (admitting an oversize one); a `>`→`>=` mutant would reject the exact max
/// too. Pin both sides: a `MAX_SUPPLY_LEN`-byte supply decodes, a `+1` one is
/// rejected.
#[test]
fn supply_length_bound_is_exclusive_at_max() {
    let max = MAX_SUPPLY_LEN as usize;

    let at_max = Answer::Supply(vec![0xAB; max]);
    let decoded = Answer::decode(&at_max.encode()).expect("a MAX_SUPPLY_LEN supply is valid");
    assert_eq!(decoded, at_max, "exactly MAX_SUPPLY_LEN bytes round-trips");

    let over = Answer::Supply(vec![0xCD; max + 1]);
    assert_eq!(
        Answer::decode(&over.encode()),
        Err(EnvError::Malformed),
        "a supply one byte over MAX_SUPPLY_LEN is rejected"
    );
}

/// `seeded.rs:65` (loop bound `out.len() < n`) and `seeded.rs:67`
/// (`take = (n - out.len()).min(8)`). The supplied vector must be **exactly** the
/// requested length:
///
/// * `<`→`==` makes the loop body never run for `n > 0` → an empty vector
///   (caught here by an exact-length assertion, *fast*, not by the hang).
/// * `<`→`<=` is a non-terminating loop (zero-progress final iteration) — it has
///   no terminating tell, so it is caught by timeout; this test still pins the
///   length so any *terminating* off-by-one is caught by assertion.
/// * `-`→`+` makes `take` saturate to 8 every iteration, overshooting the
///   request to the next multiple of 8 for any `n` that is not already one
///   (e.g. `n = 12` → 16 bytes).
#[test]
fn entropy_supply_is_exactly_the_requested_length() {
    let mut env = SeededEnv::new(0xC0FF_EE12_3456_789A, FaultPolicy::none());
    // Includes non-multiples of 8 greater than 8 (12, 20, 31, 100, 255), which
    // the `-`→`+` accumulator mutant overshoots, and small `n > 0` values that
    // the `<`→`==` bound mutant empties.
    for n in [1u32, 4, 7, 8, 9, 12, 16, 20, 31, 33, 64, 100, 255, 257] {
        let v = entropy_supply(&mut env, n);
        assert_eq!(
            v.len(),
            n as usize,
            "Entropy {{ bytes: {n} }} supplies n bytes"
        );
    }
}

/// The same exact-length contract for `Payload` (the other branch that calls
/// `supply_bytes`), so a mutation reachable only via the payload arm is pinned too.
#[test]
fn payload_supply_is_exactly_the_requested_length() {
    let mut env = SeededEnv::new(0x1357_9BDF_2468_ACE0, FaultPolicy::none());
    for n in [1u32, 9, 12, 20, 100] {
        let v = match env.decide(&P::Payload { bytes: n }) {
            Outcome::Resolved(Answer::Supply(v)) => v,
            other => panic!("payload must Supply, got {other:?}"),
        };
        assert_eq!(
            v.len(),
            n as usize,
            "Payload {{ bytes: {n} }} supplies n bytes"
        );
    }
}
