// SPDX-License-Identifier: AGPL-3.0-or-later
//! Acceptance gate 1: toy determinism property test — for an arbitrary
//! generated program and seed, two fresh spawns run to the same targets have
//! equal hashes at every checkpoint and equal final state.

use proptest::prelude::*;
use unison::toy::{ToyFactory, generate_program};
use unison::{RunOutcome, Subject, SubjectFactory, Verdict, compare_runs};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn same_seed_is_bit_identical_at_every_checkpoint(
        gen_seed in any::<u64>(),
        seed in any::<u64>(),
        checkpoint_every in 1u64..=200,
        limit in 1u64..=1200,
    ) {
        let prog = generate_program(gen_seed, 1200);
        let factory = ToyFactory { program: prog.instrs };
        let mut m1 = factory.spawn(seed);
        let mut m2 = factory.spawn(seed);

        prop_assert_eq!(m1.state_hash(), m2.state_hash(), "fresh spawns must match");

        let mut t = 0u64;
        while t < limit {
            t = (t + checkpoint_every).min(limit);
            let o1 = m1.run_to(t).unwrap();
            let o2 = m2.run_to(t).unwrap();
            prop_assert_eq!(o1, o2);
            prop_assert_eq!(m1.work(), m2.work());
            prop_assert_eq!(m1.state_hash(), m2.state_hash(), "checkpoint {} differs", t);
            if o1 == RunOutcome::Halted {
                break;
            }
        }

        // Run both to their natural halt: final state must also be identical.
        let f1 = m1.run_to(prog.work_to_halt + 10).unwrap();
        let f2 = m2.run_to(prog.work_to_halt + 10).unwrap();
        prop_assert_eq!(f1, RunOutcome::Halted);
        prop_assert_eq!(f2, RunOutcome::Halted);
        prop_assert_eq!(m1.work(), m2.work());
        prop_assert_eq!(m1.state_hash(), m2.state_hash(), "final state differs");
    }

    #[test]
    fn compare_runs_of_identical_factories_is_identical(
        gen_seed in any::<u64>(),
        seed in any::<u64>(),
        checkpoint_every in 1u64..=200,
        limit in 1u64..=1200,
    ) {
        let prog = generate_program(gen_seed, 1200);
        let a = ToyFactory { program: prog.instrs.clone() };
        let b = ToyFactory { program: prog.instrs };
        let report = compare_runs(&a, &b, seed, checkpoint_every, limit).unwrap();
        prop_assert_eq!(report.verdict, Verdict::Identical);
        // limit < work_to_halt, so the comparison must have hit the limit.
        prop_assert!(report.limit_reached);
        prop_assert_eq!(report.halted_at, None);
    }
}
