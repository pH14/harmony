// SPDX-License-Identifier: AGPL-3.0-or-later

use unison::flaky::{FlakyFactory, Perturbation};
use unison::toy::{ToyFactory, asm, generate_program};
use unison::{DivergencePoint, Verdict, bisect_divergence, compare_runs};

fn non_halting() -> ToyFactory {
    ToyFactory {
        program: generate_program(1, 500).instrs,
    }
}

#[test]
fn reached_target_checkpoints_are_counted_exactly() {
    let f = non_halting();
    let r = compare_runs(&f, &f, 3, 10, 50).unwrap();
    assert_eq!(r.verdict, Verdict::MatchingCheckpoints);
    assert!(r.limit_reached);
    assert_eq!(r.checkpoints_compared, 5);
}

#[test]
fn halt_checkpoint_is_counted_exactly() {
    let prog = vec![asm::loadi(0, 7), asm::out(0), asm::halt()];
    let f = ToyFactory { program: prog };
    let r = compare_runs(&f, &f, 9, 3, 100).unwrap();
    assert_eq!(r.verdict, Verdict::MatchingCheckpoints);
    assert_eq!(r.halted_at, Some(3));
    assert_eq!(r.checkpoints_compared, 1);
}

fn persistent(diverge_at: u64) -> (ToyFactory, FlakyFactory<ToyFactory>) {
    let prog = generate_program(5, 500).instrs;
    let toy = ToyFactory {
        program: prog.clone(),
    };
    let flaky = FlakyFactory {
        inner: ToyFactory { program: prog },
        diverge_at,
        perturb: Perturbation::XorPrng { mask: 0xABCD },
    };
    (toy, flaky)
}

#[test]
fn bisect_runs_executed_is_exact() {
    let (toy, flaky) = persistent(1);
    let p = bisect_divergence(&toy, &flaky, 9, 0, 16).unwrap();
    assert_eq!(p.first_divergent_work, 1);
    assert_eq!(p.runs_executed, 8);
}

#[test]
fn bisect_checks_initial_state_before_claiming_the_first_step() {
    let (toy, flaky) = persistent(0);
    let p = bisect_divergence(&toy, &flaky, 9, 0, 16).unwrap();
    assert_eq!(p.first_divergent_work, 0);
    assert_eq!(p.runs_executed, 2);
}

#[test]
fn deserialize_accepts_uppercase_hex() {
    let json = format!(
        r#"{{"first_divergent_work":7,"hash_a":"{a}","hash_b":"{b}","observable_digest_a":"{a}","observable_digest_b":"{b}","runs_executed":4}}"#,
        a = "AB".repeat(32),
        b = "EF".repeat(32),
    );
    let p: DivergencePoint = serde_json::from_str(&json).unwrap();
    assert_eq!(p.hash_a, [0xAB; 32]);
    assert_eq!(p.hash_b, [0xEF; 32]);
    assert_eq!(p.observable_digest_a, [0xAB; 32]);
    assert_eq!(p.observable_digest_b, [0xEF; 32]);
    assert_eq!(p.first_divergent_work, 7);
    assert_eq!(p.runs_executed, 4);
}
