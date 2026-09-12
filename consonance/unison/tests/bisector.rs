// SPDX-License-Identifier: AGPL-3.0-or-later

use proptest::prelude::*;
use unison::flaky::{FlakyFactory, Perturbation};
use unison::toy::{ToyFactory, asm, generate_program};
use unison::{SubjectError, Verdict, bisect_divergence, compare_runs};

const MIN_WORK: u64 = 1024;

fn pair(
    gen_seed: u64,
    diverge_at: u64,
    perturb: Perturbation,
) -> (ToyFactory, FlakyFactory<ToyFactory>) {
    let prog = generate_program(gen_seed, MIN_WORK);
    let toy = ToyFactory {
        program: prog.instrs.clone(),
    };
    let flaky = FlakyFactory {
        inner: ToyFactory {
            program: prog.instrs,
        },
        diverge_at,
        perturb,
    };
    (toy, flaky)
}

fn ceil_log2(n: u64) -> u64 {
    u64::from(64 - n.saturating_sub(1).leading_zeros())
}

fn assert_bisects_exactly(
    toy: &ToyFactory,
    flaky: &FlakyFactory<ToyFactory>,
    seed: u64,
    checkpoint_every: u64,
    limit: u64,
    diverge_at: u64,
) -> Result<(), TestCaseError> {
    let report = compare_runs(toy, flaky, seed, checkpoint_every, limit).unwrap();
    match report.verdict {
        Verdict::Diverged {
            last_match,
            first_mismatch,
        } => {
            let lo = last_match.unwrap_or(0);
            prop_assert!(
                lo < diverge_at && diverge_at <= first_mismatch,
                "bracket ({}, {}] does not contain diverge_at {}",
                lo,
                first_mismatch,
                diverge_at
            );
            let point = bisect_divergence(toy, flaky, seed, lo, first_mismatch).unwrap();
            prop_assert_eq!(
                point.first_divergent_work,
                diverge_at,
                "bisector missed: bracket ({}, {}]",
                lo,
                first_mismatch
            );
            prop_assert_ne!(point.hash_a, point.hash_b);
            let bound = 2 * (ceil_log2(first_mismatch - lo) + 2);
            prop_assert!(
                point.runs_executed <= bound,
                "runs_executed {} exceeds bound {}",
                point.runs_executed,
                bound
            );
        }
        other => prop_assert!(false, "expected Diverged, got {:?}", other),
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn bisector_finds_exact_divergence_point(
        gen_seed in any::<u64>(),
        seed in any::<u64>(),
        checkpoint_every in 1u64..=200,
        limit in 32u64..=1000,
        selector in 0u8..8,
        raw in any::<u64>(),
        mask in any::<u64>(),
    ) {
        let k_max = (limit / checkpoint_every).max(1);
        let k = (raw % k_max).max(1);
        let boundary = k * checkpoint_every;
        let diverge_at = match selector {
            0 => 1,
            1 => limit - 1,
            2 => limit,
            3 => boundary.min(limit),
            4 => (boundary + 1).min(limit),
            5 => boundary.saturating_sub(1).clamp(1, limit),
            _ => raw % limit + 1,
        };
        let perturb = Perturbation::XorPrng { mask: mask | 1 };
        let (toy, flaky) = pair(gen_seed, diverge_at, perturb);
        assert_bisects_exactly(&toy, &flaky, seed, checkpoint_every, limit, diverge_at)?;
    }
}

#[test]
fn bisector_exact_on_directed_edge_cases() {
    let (checkpoint_every, limit) = (64u64, 512u64);
    for diverge_at in [1, 63, 64, 65, 127, 128, 129, 448, 511, 512] {
        let perturb = Perturbation::XorPrng {
            mask: 0xDEC0_DE00_0000_0001,
        };
        let (toy, flaky) = pair(11, diverge_at, perturb);
        assert_bisects_exactly(&toy, &flaky, 77, checkpoint_every, limit, diverge_at)
            .unwrap_or_else(|e| panic!("diverge_at {diverge_at}: {e}"));
    }
}

#[test]
fn bisector_exact_for_register_perturbation() {
    let program = vec![
        asm::loadi(7, 200),
        asm::rand(1),
        asm::out(1),
        asm::add(2, 0),
        asm::loadi(6, 1),
        asm::sub(7, 6),
        asm::jnz(7, 1),
        asm::halt(),
    ];
    let toy = ToyFactory {
        program: program.clone(),
    };
    let flaky = FlakyFactory {
        inner: ToyFactory { program },
        diverge_at: 37,
        perturb: Perturbation::XorReg {
            reg: 0,
            mask: 0xFF00_FF00,
        },
    };
    assert_bisects_exactly(&toy, &flaky, 5, 50, 400, 37).unwrap();
}

#[test]
fn forced_early_halt_yields_halt_mismatch() {
    let (toy, flaky) = pair(3, 137, Perturbation::ForceHalt);
    let report = compare_runs(&toy, &flaky, 9, 50, 1000).unwrap();
    assert_eq!(
        report.verdict,
        Verdict::HaltMismatch {
            a: None,
            b: Some(137)
        }
    );
    assert_eq!(report.halted_at, None);
    assert!(!report.limit_reached);

    let report = compare_runs(&flaky, &toy, 9, 50, 1000).unwrap();
    assert_eq!(
        report.verdict,
        Verdict::HaltMismatch {
            a: Some(137),
            b: None
        }
    );

    let prog = generate_program(3, 100);
    let toy = ToyFactory {
        program: prog.instrs.clone(),
    };
    let flaky = FlakyFactory {
        inner: ToyFactory {
            program: prog.instrs,
        },
        diverge_at: 37,
        perturb: Perturbation::ForceHalt,
    };
    let limit = prog.work_to_halt + 50;
    let report = compare_runs(&toy, &flaky, 9, limit, limit).unwrap();
    assert_eq!(
        report.verdict,
        Verdict::HaltMismatch {
            a: Some(prog.work_to_halt),
            b: Some(37)
        }
    );
}

#[test]
fn no_divergence_path() {
    let (toy, flaky) = pair(21, u64::MAX, Perturbation::XorPrng { mask: 0xBAD });

    let report = compare_runs(&toy, &flaky, 13, 64, 800).unwrap();
    assert_eq!(report.verdict, Verdict::Identical);
    assert!(report.limit_reached);
    assert_eq!(report.halted_at, None);

    let halt = generate_program(21, MIN_WORK).work_to_halt;
    let report = compare_runs(&toy, &flaky, 13, 64, halt + 100).unwrap();
    assert_eq!(report.verdict, Verdict::Identical);
    assert!(!report.limit_reached);
    assert_eq!(report.halted_at, Some(halt));

    assert_eq!(
        bisect_divergence(&toy, &flaky, 13, 0, 800),
        Err(SubjectError::NoDivergence { hi: 800 })
    );
}
