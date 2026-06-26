// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests that exist specifically to kill mutants `cargo mutants -p unison`
//! left surviving — lines the rest of the suite executes but does not actually
//! constrain. Each test names the mutant(s) it removes (file:line, see
//! `unison/IMPLEMENTATION.md` "Mutation testing").
//!
//! The recurring gap these close: the existing suite asserts *verdicts* and
//! *upper bounds* (e.g. `runs_executed <= bound`) but rarely an *exact count*,
//! so a counter mutated to stay at zero (`+= 1` → `*= 1`) sails through. These
//! pin the counts down.

use unison::flaky::{FlakyFactory, Perturbation};
use unison::toy::{ToyFactory, asm, generate_program};
use unison::{DivergencePoint, Verdict, bisect_divergence, compare_runs};

fn non_halting() -> ToyFactory {
    // work_to_halt > 500, so nothing halts inside the small windows below.
    ToyFactory {
        program: generate_program(1, 500).instrs,
    }
}

// Kills lib.rs:184 `checkpoints_compared += 1` → `*= 1` (the ReachedTarget
// arm): with `*=` the counter is pinned at 0, so only an *exact* count notices.
#[test]
fn reached_target_checkpoints_are_counted_exactly() {
    let f = non_halting();
    let r = compare_runs(&f, &f, 3, 10, 50).unwrap();
    assert_eq!(r.verdict, Verdict::Identical);
    assert!(r.limit_reached);
    // Checkpoints at 10, 20, 30, 40, 50 — five matched comparisons.
    assert_eq!(r.checkpoints_compared, 5);
}

// Kills lib.rs:210 `checkpoints_compared += 1` → `-= 1` / `*= 1` (the
// Halted/Halted arm). The program halts at work 3 and the checkpoint interval
// (3) makes the *first* comparison the halt one, so the ReachedTarget arm
// (line 184) never runs: the count is established solely by line 210.
#[test]
fn halt_checkpoint_is_counted_exactly() {
    let prog = vec![asm::loadi(0, 7), asm::out(0), asm::halt()]; // halts at work 3
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
        // PRNG perturbation: the xorshift64* update is a bijection, so the
        // divergence is permanent and the bracket math below is exact.
        perturb: Perturbation::XorPrng { mask: 0xABCD },
    };
    (toy, flaky)
}

// Kills lib.rs:302 and lib.rs:305 `runs_executed += 1` → `*= 1` (one per
// probed machine). The existing property test only asserts `runs_executed <=
// bound`, which 0 trivially satisfies; this pins the *exact* probe count.
//
// Bracket (0, 16] with a divergence at work 1: probe(16) then mids 8, 4, 2, 1
// — 5 probes × 2 machines = 10 individual executions.
#[test]
fn bisect_runs_executed_is_exact() {
    let (toy, flaky) = persistent(1);
    let p = bisect_divergence(&toy, &flaky, 9, 0, 16).unwrap();
    assert_eq!(p.first_divergent_work, 1);
    assert_eq!(p.runs_executed, 10);
}

// Kills lib.rs:312 `if lo > 0` → `if lo >= 0`. With `lo == 0` the start of time
// is trusted, so a machine that already differs at spawn (diverge_at == 0)
// bisects to work 1; the mutant instead probes lo == 0, sees the spawn-time
// difference, and wrongly returns `DivergesAtLo` — turning an Ok into an Err.
#[test]
fn bisect_trusts_lo_zero_as_start_of_time() {
    let (toy, flaky) = persistent(0);
    let p = bisect_divergence(&toy, &flaky, 9, 0, 16).unwrap();
    assert_eq!(p.first_divergent_work, 1);
}

// Kills the uppercase-hex arm mutants in `hex32::deserialize::nibble`
// (lib.rs:360): the deleted `b'A'..=b'F'` arm and its `c - b'A' + 10`
// arithmetic. The serializer only ever emits lowercase, so the round-trip
// tests never feed an uppercase digit; deserialize must still accept it.
#[test]
fn deserialize_accepts_uppercase_hex() {
    let json = format!(
        r#"{{"first_divergent_work":7,"hash_a":"{a}","hash_b":"{b}","runs_executed":4}}"#,
        a = "AB".repeat(32), // 0xAB exercises nibble('A')=10, nibble('B')=11
        b = "EF".repeat(32), // 0xEF exercises nibble('E')=14, nibble('F')=15
    );
    let p: DivergencePoint = serde_json::from_str(&json).unwrap();
    assert_eq!(p.hash_a, [0xAB; 32]);
    assert_eq!(p.hash_b, [0xEF; 32]);
    assert_eq!(p.first_divergent_work, 7);
    assert_eq!(p.runs_executed, 4);
}
