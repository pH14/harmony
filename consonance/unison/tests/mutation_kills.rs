// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests that exist specifically to kill mutants `cargo mutants -p unison`
//! left surviving — lines the rest of the suite executes but does not actually
//! constrain. Each test names the mutant(s) it removes (file:line, see
//! `unison/README.md` "Mutation testing").
//!
//! The recurring gap these close: the existing suite asserts *verdicts* and
//! *upper bounds* (e.g. `runs_executed <= bound`) but rarely an *exact count*,
//! so a counter mutated to stay at zero (`+= 1` → `*= 1`) sails through. These
//! pin the counts down.

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
    assert_eq!(r.verdict, Verdict::Identical);
    assert!(r.limit_reached);
    assert_eq!(r.checkpoints_compared, 5);
}

#[test]
fn halt_checkpoint_is_counted_exactly() {
    let prog = vec![asm::loadi(0, 7), asm::out(0), asm::halt()];
    let f = ToyFactory { program: prog };
    let r = compare_runs(&f, &f, 9, 3, 100).unwrap();
    assert_eq!(r.verdict, Verdict::Identical);
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
    assert_eq!(p.runs_executed, 10);
}

#[test]
fn bisect_trusts_lo_zero_as_start_of_time() {
    let (toy, flaky) = persistent(0);
    let p = bisect_divergence(&toy, &flaky, 9, 0, 16).unwrap();
    assert_eq!(p.first_divergent_work, 1);
}

#[test]
fn deserialize_accepts_uppercase_hex() {
    let json = format!(
        r#"{{"first_divergent_work":7,"hash_a":"{a}","hash_b":"{b}","runs_executed":4}}"#,
        a = "AB".repeat(32),
        b = "EF".repeat(32),
    );
    let p: DivergencePoint = serde_json::from_str(&json).unwrap();
    assert_eq!(p.hash_a, [0xAB; 32]);
    assert_eq!(p.hash_b, [0xEF; 32]);
    assert_eq!(p.first_divergent_work, 7);
    assert_eq!(p.runs_executed, 4);
}
