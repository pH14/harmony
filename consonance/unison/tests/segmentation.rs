// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cell::Cell;
use std::rc::Rc;
use unison::flaky::{FlakyFactory, Perturbation};
use unison::toy::{ToyFactory, generate_program};
use unison::{
    RunOutcome, Subject, SubjectError, SubjectFactory, Verdict, bisect_divergence, compare_runs,
    first_divergence_from_report,
};

#[derive(Clone)]
struct CounterFactory(Rc<Cell<u8>>);

struct CounterMachine {
    work: u64,
    spawn_id: u8,
}

impl SubjectFactory for CounterFactory {
    type M = CounterMachine;

    fn spawn(&self, _: u64) -> Self::M {
        let spawn_id = self.0.get();
        self.0.set(spawn_id.wrapping_add(1));
        CounterMachine { work: 0, spawn_id }
    }
}

impl Subject for CounterMachine {
    fn run_to(&mut self, target: u64) -> Result<RunOutcome, SubjectError> {
        self.work = target;
        Ok(RunOutcome::ReachedTarget)
    }

    fn work(&self) -> u64 {
        self.work
    }

    fn state_hash(&self) -> Result<[u8; 32], SubjectError> {
        let mut hash = [self.work as u8; 32];
        if self.work > 0 {
            hash[0] ^= self.spawn_id;
        }
        Ok(hash)
    }

    fn observable_digest(&self) -> [u8; 32] {
        [0; 32]
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Plain,
    ThirdCall,
    DirectLow,
}

#[derive(Clone, Copy)]
struct Factory(Mode);

struct Machine {
    work: u64,
    calls: u64,
    output: u8,
    mode: Mode,
}

impl SubjectFactory for Factory {
    type M = Machine;

    fn spawn(&self, _: u64) -> Self::M {
        Machine {
            work: 0,
            calls: 0,
            output: 0,
            mode: self.0,
        }
    }
}

impl Subject for Machine {
    fn run_to(&mut self, target: u64) -> Result<RunOutcome, SubjectError> {
        if target < self.work {
            return Err(SubjectError::TargetBehind {
                target,
                current: self.work,
            });
        }
        self.calls += 1;
        self.work = target;
        if matches!(self.mode, Mode::ThirdCall) && self.calls >= 3
            || matches!(self.mode, Mode::DirectLow)
                && (target == 4 || target == 2 && self.calls == 1)
        {
            self.output = 1;
        }
        Ok(RunOutcome::ReachedTarget)
    }

    fn work(&self) -> u64 {
        self.work
    }

    fn state_hash(&self) -> Result<[u8; 32], SubjectError> {
        Ok([self.work as u8; 32])
    }

    fn observable_digest(&self) -> [u8; 32] {
        [self.output; 32]
    }
}

#[test]
fn compare_refuses_a_mismatch_missing_from_direct_replay() {
    let a = Factory(Mode::Plain);
    let b = Factory(Mode::ThirdCall);
    assert_eq!(
        bisect_divergence(&a, &b, 7, 2, 4),
        Err(SubjectError::NoDivergence { hi: 4 })
    );
    let report = compare_runs(&a, &b, 7, 2, 4).unwrap();
    assert_eq!(
        report.verdict,
        Verdict::UnreplayableDivergence { observed_at: 4 }
    );
}

#[test]
fn compare_refuses_a_matching_low_checkpoint_that_direct_replay_diverges_at() {
    let a = Factory(Mode::Plain);
    let b = Factory(Mode::DirectLow);
    assert_eq!(
        bisect_divergence(&a, &b, 7, 2, 4),
        Err(SubjectError::DivergesAtLo { lo: 2 })
    );
    let report = compare_runs(&a, &b, 7, 2, 4).unwrap();
    assert_eq!(
        report.verdict,
        Verdict::UnreplayableDivergence { observed_at: 4 }
    );
}

#[test]
fn same_seed_nondeterminism_is_reported_without_a_first_work_claim() {
    let factory = CounterFactory(Rc::new(Cell::new(0)));
    let report = compare_runs(&factory, &factory, 7, 1, 1).unwrap();
    assert_eq!(
        report.verdict,
        Verdict::UnreplayableDivergence { observed_at: 1 }
    );
    assert_eq!(
        serde_json::to_value(&report).unwrap()["verdict"]["unreplayable_divergence"]["observed_at"],
        1
    );
    assert_eq!(
        first_divergence_from_report(&factory, &factory, 7, &report).unwrap(),
        None
    );
}

fn assert_segmentation_invariant<F: SubjectFactory>(factory: &F, halt_work: u64) {
    for target in [0, 1, 20, 37, 100, halt_work, halt_work + 1] {
        let mut direct = factory.spawn(7);
        let direct_outcome = direct.run_to(target).unwrap();
        let mut segmented = factory.spawn(7);
        for cut in [0, 1, 7, 20, 36, 37, 100] {
            if cut < target {
                segmented.run_to(cut).unwrap();
            }
        }
        let segmented_outcome = segmented.run_to(target).unwrap();
        assert_eq!(segmented_outcome, direct_outcome);
        assert_eq!(segmented.work(), direct.work());
        assert_eq!(
            segmented.state_hash().unwrap(),
            direct.state_hash().unwrap()
        );
        assert_eq!(segmented.observable_digest(), direct.observable_digest());
    }
}

#[test]
fn bundled_subjects_are_segmentation_invariant() {
    let program = generate_program(11, 200);
    let toy = ToyFactory {
        program: program.instrs,
    };
    assert_segmentation_invariant(&toy, program.work_to_halt);
    let flaky = FlakyFactory {
        inner: toy,
        diverge_at: 37,
        perturb: Perturbation::XorPrng { mask: 0xABCD },
    };
    assert_segmentation_invariant(&flaky, program.work_to_halt);
}
