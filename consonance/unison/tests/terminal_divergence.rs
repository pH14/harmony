// SPDX-License-Identifier: AGPL-3.0-or-later

use unison::{
    RunOutcome, Subject, SubjectError, SubjectFactory, Verdict, bisect_divergence, compare_runs,
    first_divergence_from_report,
};

#[derive(Clone, Copy)]
struct Factory(bool);

struct Machine {
    work: u64,
    halted: bool,
    terminal_difference: bool,
}

#[derive(Clone, Copy)]
struct SpawnHaltedFactory(bool, u64);

impl SubjectFactory for SpawnHaltedFactory {
    type M = Machine;

    fn spawn(&self, _: u64) -> Machine {
        Machine {
            work: self.1,
            halted: true,
            terminal_difference: self.0,
        }
    }
}

impl SubjectFactory for Factory {
    type M = Machine;

    fn spawn(&self, _: u64) -> Machine {
        Machine {
            work: 0,
            halted: false,
            terminal_difference: self.0,
        }
    }
}

impl Subject for Machine {
    fn run_to(&mut self, target: u64) -> Result<RunOutcome, SubjectError> {
        if target < self.work && !self.halted {
            return Err(SubjectError::TargetBehind {
                target,
                current: self.work,
            });
        }
        if self.halted {
            return Ok(RunOutcome::Halted);
        }
        if target <= 1 && !self.halted {
            self.work = target;
            Ok(RunOutcome::ReachedTarget)
        } else {
            self.work = 1;
            self.halted = true;
            Ok(RunOutcome::Halted)
        }
    }

    fn work(&self) -> u64 {
        self.work
    }

    fn state_hash(&self) -> Result<[u8; 32], SubjectError> {
        Ok([self.work as u8
            | u8::from(self.halted) << 6
            | u8::from(self.halted && self.terminal_difference) << 7; 32])
    }

    fn observable_digest(&self) -> [u8; 32] {
        [0; 32]
    }
}

#[test]
fn same_work_terminal_change_cannot_be_reported_as_a_bisection_bracket() {
    let report = compare_runs(&Factory(false), &Factory(true), 7, 1, 2).unwrap();
    assert_eq!(report.halted_at, Some(1));
    assert_eq!(
        report.verdict,
        Verdict::TerminalDiverged {
            last_match: Some(1),
            halted_at: 1,
        }
    );
    assert_eq!(
        first_divergence_from_report(&Factory(false), &Factory(true), 7, &report).unwrap(),
        None
    );
}

#[test]
fn terminal_change_below_next_checkpoint_is_not_a_bisection_bracket() {
    let report = compare_runs(&Factory(false), &Factory(true), 7, 2, 2).unwrap();
    assert_eq!(report.halted_at, Some(1));
    assert_eq!(
        report.verdict,
        Verdict::TerminalDiverged {
            last_match: None,
            halted_at: 1,
        }
    );
}

#[test]
fn direct_bisection_refuses_a_divergence_after_both_halted_below_the_probe() {
    assert_eq!(
        bisect_divergence(&Factory(false), &Factory(true), 7, 0, 2),
        Err(SubjectError::UnreplayableTerminal {
            target: 2,
            actual: 1,
        })
    );
}

#[test]
fn equal_spawn_halt_counts_with_different_observations_have_no_work_zero_bracket() {
    let report = compare_runs(
        &SpawnHaltedFactory(false, 1),
        &SpawnHaltedFactory(true, 1),
        7,
        1,
        2,
    )
    .unwrap();
    assert_eq!(
        report.verdict,
        Verdict::TerminalDiverged {
            last_match: None,
            halted_at: 1,
        }
    );
    assert_eq!(report.halted_at, Some(1));
    assert_eq!(report.checkpoints_compared, 0);
}

#[test]
fn equal_spawn_halt_counts_at_zero_remain_directly_bisectable() {
    let a = SpawnHaltedFactory(false, 0);
    let b = SpawnHaltedFactory(true, 0);
    let report = compare_runs(&a, &b, 7, 1, 2).unwrap();
    assert_eq!(
        report.verdict,
        Verdict::Diverged {
            last_match: None,
            first_mismatch: 0,
        }
    );
    assert_eq!(report.halted_at, Some(0));
    assert_eq!(
        bisect_divergence(&a, &b, 7, 0, 0)
            .unwrap()
            .first_divergent_work,
        0
    );
}
