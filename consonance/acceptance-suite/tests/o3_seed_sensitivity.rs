// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — O3 seed-sensitivity, the anti-cheat gate, both directions.
//!
//! Two crafted toy programs, plus two negative variants built by a faithful
//! program transformation over the real `ToyMachine` (no re-implemented VM):
//!
//! * **RNG, honest** — `RAND r0; OUT r0; HALT`: control-flow-stable and
//!   RNG-consuming. `check_seed_sensitivity(rng_consuming: true)` must PASS
//!   (`work_a == work_b`, `out_a != out_b`).
//! * **RNG, faked determinism** — the same program with `RAND` stubbed to a
//!   constant (`RAND rd` → `LOADI rd, K`, identical work + control flow). Output
//!   no longer varies with the seed, so the oracle must FAIL.
//! * **Pure** — `LOADI r0,K; OUT r0; HALT`: no RNG. `rng_consuming: false` must
//!   PASS (`out_a == out_b`).
//! * **Pure, seed-leaked** — the pure program with a `RAND r5; OUT r5` prefix
//!   that leaks the seed into observable output, so `rng_consuming: false` must
//!   FAIL.
//!
//! Distinct **nonzero** seeds are used so the two effective PRNG states differ
//! (`spawn(0)` aliases `spawn(ZERO_SEED_STATE)`); the xorshift64* output is then
//! guaranteed to differ, making every direction a hard guarantee, not a
//! probabilistic one.

use acceptance_suite::check_seed_sensitivity;
use proptest::prelude::*;
use unison::toy::{Instr, ToyFactory, asm};

const STUB_CONST: u64 = 0xA5A5_A5A5_A5A5_A5A5;
const LIMIT: u64 = 1_000;

/// `RAND r0; OUT r0; HALT` — RNG-consuming, control-flow-stable.
fn rng_program() -> Vec<Instr> {
    vec![asm::rand(0), asm::out(0), asm::halt()]
}

/// `LOADI r0,K; OUT r0; HALT` — pure, no RNG.
fn pure_program() -> Vec<Instr> {
    vec![asm::loadi(0, 0xDEAD_BEEF), asm::out(0), asm::halt()]
}

/// Replace every `RAND rd` with `LOADI rd, STUB_CONST` — RAND wired to a
/// constant, preserving work count and control flow exactly.
fn stub_rand(prog: &[Instr]) -> Vec<Instr> {
    prog.iter()
        .map(|i| match *i {
            Instr::Rand { rd } => Instr::Loadi {
                rd,
                imm: STUB_CONST,
            },
            other => other,
        })
        .collect()
}

/// Prepend `RAND r5; OUT r5` so the seed reaches observable output.
fn leak_seed(prog: &[Instr]) -> Vec<Instr> {
    let mut out = vec![asm::rand(5), asm::out(5)];
    out.extend_from_slice(prog);
    out
}

fn factory(prog: Vec<Instr>) -> ToyFactory {
    ToyFactory { program: prog }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    // ---- direction 1: RNG-consuming ------------------------------------------

    #[test]
    fn rng_consuming_passes_and_faked_determinism_fails(
        a in 1u64..=u64::MAX,
        b in 1u64..=u64::MAX,
    ) {
        prop_assume!(a != b);

        // Honest RNG payload: seed-stable work, seed-varying output → PASS.
        let honest = check_seed_sensitivity(&factory(rng_program()), a, b, LIMIT, true).unwrap();
        prop_assert!(honest.passed, "honest RNG payload must pass O3: {honest:?}");

        // RAND stubbed to a constant (faked determinism) → FAIL.
        let faked = factory(stub_rand(&rng_program()));
        let res = check_seed_sensitivity(&faked, a, b, LIMIT, true).unwrap();
        prop_assert!(!res.passed, "faked-determinism payload must fail O3: {res:?}");
        prop_assert!(res.detail.contains("faked determinism"), "{}", res.detail);
    }

    // ---- direction 2: pure ----------------------------------------------------

    #[test]
    fn pure_passes_and_seed_leak_fails(
        a in 1u64..=u64::MAX,
        b in 1u64..=u64::MAX,
    ) {
        prop_assume!(a != b);

        // Pure payload: identical observable output across seeds → PASS.
        let pure = check_seed_sensitivity(&factory(pure_program()), a, b, LIMIT, false).unwrap();
        prop_assert!(pure.passed, "pure payload must pass O3: {pure:?}");

        // Seed leaked into observable output → FAIL.
        let leaky = factory(leak_seed(&pure_program()));
        let res = check_seed_sensitivity(&leaky, a, b, LIMIT, false).unwrap();
        prop_assert!(!res.passed, "seed-leaking payload must fail O3: {res:?}");
        prop_assert!(res.detail.contains("seed leak"), "{}", res.detail);
    }
}

#[test]
fn equal_seeds_is_a_fail_not_a_panic() {
    let res = check_seed_sensitivity(&factory(rng_program()), 42, 42, LIMIT, true).unwrap();
    assert!(!res.passed);
    assert!(res.detail.contains("distinct seeds"), "{}", res.detail);
}

/// Finding 4: a non-halting RNG payload must NOT pass O3. It consumes RNG (so
/// `out_a != out_b`) and never halts, so both runs are capped at `limit` with
/// `work_a == work_b == limit` — which would make the work-stability clause true
/// *by construction*. Requiring both runs to halt rejects this bounded prefix.
#[test]
fn non_halting_divergence_does_not_pass() {
    // pc0: RAND r0; OUT r0; LOADI r1,1; JNZ r1 -> pc0  (infinite, RNG-consuming)
    let looping = vec![
        asm::rand(0),
        asm::out(0),
        asm::loadi(1, 1),
        Instr::Jnz { rs: 1, target: 0 },
    ];
    let res = check_seed_sensitivity(&factory(looping), 1, 2, LIMIT, true).unwrap();
    assert!(
        !res.passed,
        "a never-halting payload must not pass O3 (bounded-prefix work equality): {res:?}"
    );
    assert!(
        res.detail.contains("inconclusive") && res.detail.contains("halt"),
        "{}",
        res.detail
    );
}
