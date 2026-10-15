// SPDX-License-Identifier: AGPL-3.0-or-later

use proptest::prelude::*;
use unison::toy::{ToyFactory, generate_program};
use unison::{
    RunOutcome, Subject, SubjectError, SubjectFactory, Verdict, bisect_divergence, compare_runs,
};

struct RoundedFactory {
    quantum: u64,
    halt_at: Option<u64>,
}

struct RoundedSubject {
    quantum: u64,
    work: u64,
    halt_at: Option<u64>,
}

impl SubjectFactory for RoundedFactory {
    type M = RoundedSubject;

    fn spawn(&self, _: u64) -> Self::M {
        RoundedSubject {
            quantum: self.quantum,
            work: 0,
            halt_at: self.halt_at,
        }
    }
}

impl Subject for RoundedSubject {
    fn run_to(&mut self, target: u64) -> Result<RunOutcome, SubjectError> {
        if target < self.work {
            return Err(SubjectError::TargetBehind {
                target,
                current: self.work,
            });
        }
        self.work = target.div_ceil(self.quantum) * self.quantum;
        Ok(
            if self.halt_at.is_some_and(|halt_at| self.work >= halt_at) {
                RunOutcome::Halted
            } else {
                RunOutcome::ReachedTarget
            },
        )
    }

    fn work(&self) -> u64 {
        self.work
    }

    fn state_hash(&self) -> Result<[u8; 32], SubjectError> {
        Ok([0; 32])
    }

    fn observable_digest(&self) -> [u8; 32] {
        [0; 32]
    }
}

#[test]
fn comparison_refuses_a_subject_that_overshoots_its_reported_target() {
    let exact = RoundedFactory {
        quantum: 1,
        halt_at: None,
    };
    let rounded = RoundedFactory {
        quantum: 3,
        halt_at: None,
    };
    assert!(matches!(
        compare_runs(&exact, &rounded, 0, 1, 1),
        Err(SubjectError::InconsistentWork {
            target: 1,
            actual: 3,
            outcome: RunOutcome::ReachedTarget,
        })
    ));
    assert!(matches!(
        bisect_divergence(&exact, &rounded, 0, 0, 1),
        Err(SubjectError::InconsistentWork {
            target: 1,
            actual: 3,
            outcome: RunOutcome::ReachedTarget,
        })
    ));
}

#[test]
fn comparison_refuses_a_subject_that_halts_after_the_requested_target() {
    let exact = RoundedFactory {
        quantum: 1,
        halt_at: Some(3),
    };
    let rounded = RoundedFactory {
        quantum: 3,
        halt_at: Some(3),
    };
    assert!(matches!(
        compare_runs(&exact, &rounded, 0, 1, 3),
        Err(SubjectError::InconsistentWork {
            target: 1,
            actual: 3,
            outcome: RunOutcome::Halted,
        })
    ));
    assert!(matches!(
        bisect_divergence(&exact, &rounded, 0, 0, 1),
        Err(SubjectError::InconsistentWork {
            target: 1,
            actual: 3,
            outcome: RunOutcome::Halted,
        })
    ));
}

struct RegressingFactory;

struct RegressingSubject {
    work: u64,
}

impl SubjectFactory for RegressingFactory {
    type M = RegressingSubject;

    fn spawn(&self, _: u64) -> Self::M {
        RegressingSubject { work: 0 }
    }
}

impl Subject for RegressingSubject {
    fn run_to(&mut self, target: u64) -> Result<RunOutcome, SubjectError> {
        if target < 2 {
            self.work = target;
            Ok(RunOutcome::ReachedTarget)
        } else {
            self.work = 0;
            Ok(RunOutcome::Halted)
        }
    }

    fn work(&self) -> u64 {
        self.work
    }

    fn state_hash(&self) -> Result<[u8; 32], SubjectError> {
        Ok([0; 32])
    }

    fn observable_digest(&self) -> [u8; 32] {
        [0; 32]
    }
}

#[test]
fn comparison_refuses_a_subject_whose_work_regresses_on_halt() {
    assert_eq!(
        compare_runs(&RegressingFactory, &RegressingFactory, 0, 1, 2),
        Err(SubjectError::InconsistentWork {
            target: 2,
            actual: 0,
            outcome: RunOutcome::Halted,
        })
    );
}

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

        prop_assert_eq!(m1.state_hash().unwrap(), m2.state_hash().unwrap(), "fresh spawns must match");

        let mut t = 0u64;
        while t < limit {
            t = (t + checkpoint_every).min(limit);
            let o1 = m1.run_to(t).unwrap();
            let o2 = m2.run_to(t).unwrap();
            prop_assert_eq!(o1, o2);
            prop_assert_eq!(m1.work(), m2.work());
            prop_assert_eq!(m1.state_hash().unwrap(), m2.state_hash().unwrap(), "checkpoint {} differs", t);
            if o1 == RunOutcome::Halted {
                break;
            }
        }

        let f1 = m1.run_to(prog.work_to_halt + 10).unwrap();
        let f2 = m2.run_to(prog.work_to_halt + 10).unwrap();
        prop_assert_eq!(f1, RunOutcome::Halted);
        prop_assert_eq!(f2, RunOutcome::Halted);
        prop_assert_eq!(m1.work(), m2.work());
        prop_assert_eq!(m1.state_hash().unwrap(), m2.state_hash().unwrap(), "final state differs");
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
        prop_assert_eq!(report.verdict, Verdict::MatchingCheckpoints);
        prop_assert!(report.limit_reached);
        prop_assert_eq!(report.halted_at, None);
    }
}
