// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use fault_policy::{
    Action, Answer, BlockOp, DecisionPoint as P, EnvError, Environment, Fault, FaultPolicy,
    MAX_SUPPLY_LEN, Moment, Outcome, SeededEnv,
};

fn entropy_supply(env: &mut SeededEnv, n: u32) -> Vec<u8> {
    match env.decide(&P::Entropy { bytes: n }) {
        Outcome::Resolved(Answer::Supply(v)) => v,
        other => panic!("entropy must Supply, got {other:?}"),
    }
}

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

    let at_bound = supply(5);
    let mut env = recorded_with_override(7, 0, at_bound);
    env.set_moment(0);
    let mut base = SeededEnv::new(7, FaultPolicy::none());
    assert_eq!(
        env.decide(&p),
        base.decide(&p),
        "override selecting exactly `ready` is ignored; base answers"
    );

    let in_range = supply(4);
    let mut env2 = recorded_with_override(7, 0, in_range.clone());
    env2.set_moment(0);
    assert_eq!(
        env2.decide(&p),
        Outcome::Resolved(in_range),
        "an in-range override (4 < 5) wins"
    );
}

#[test]
fn block_torn_bound_is_inclusive_at_len() {
    let len = 8u32;
    let io = P::BlockIo {
        op: BlockOp::Write,
        lba: 0,
        len,
    };
    let torn = |n: u32| Answer::Fault(Fault::BlockTorn(n));

    assert!(
        io.admits(&torn(0)),
        "tearing off 0 bytes (< len) is admissible"
    );
    assert!(io.admits(&torn(len - 1)), "n = len-1 (< len) is admissible");
    assert!(
        io.admits(&torn(len)),
        "n = len (the inclusive bound) is admissible — a full-length tear"
    );
    assert!(
        !io.admits(&torn(len + 1)),
        "n = len+1 (> len) tears off more than the request: inadmissible"
    );

    assert!(io.admits(&Answer::Fault(Fault::BlockEio)));
    assert!(io.admits(&Answer::Fault(Fault::BlockNospc)));
}

fn recorded_with_override(seed: u64, at: Moment, ans: Answer) -> fault_policy::RecordedEnv {
    fault_policy::EnvSpec::Recorded {
        seed,
        policy: FaultPolicy::none(),
        overrides: BTreeMap::from([(at, Action::Guest(ans))]),
        standing: vec![],
        reseeds: std::collections::BTreeMap::new(),
        payloads: None,
    }
    .materialize()
}

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

#[test]
fn entropy_supply_is_exactly_the_requested_length() {
    let mut env = SeededEnv::new(0xC0FF_EE12_3456_789A, FaultPolicy::none());
    for n in [1u32, 4, 7, 8, 9, 12, 16, 20, 31, 33, 64, 100, 255, 257] {
        let v = entropy_supply(&mut env, n);
        assert_eq!(
            v.len(),
            n as usize,
            "Entropy {{ bytes: {n} }} supplies n bytes"
        );
    }
}

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
