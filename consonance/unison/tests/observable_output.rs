// SPDX-License-Identifier: AGPL-3.0-or-later

use unison::{
    RunOutcome, Subject, SubjectError, SubjectFactory, Verdict, bisect_divergence, compare_runs,
};

#[derive(Clone, Copy)]
struct OutputFactory {
    output: u8,
    output_at: u64,
    halt_at: Option<u64>,
}

struct OutputMachine {
    factory: OutputFactory,
    work: u64,
}

impl SubjectFactory for OutputFactory {
    type M = OutputMachine;

    fn spawn(&self, _: u64) -> Self::M {
        OutputMachine {
            factory: *self,
            work: 0,
        }
    }
}

impl Subject for OutputMachine {
    fn run_to(&mut self, target: u64) -> Result<RunOutcome, SubjectError> {
        if target < self.work {
            return Err(SubjectError::TargetBehind {
                target,
                current: self.work,
            });
        }
        self.work = target.min(self.factory.halt_at.unwrap_or(u64::MAX));
        Ok(if self.factory.halt_at == Some(self.work) {
            RunOutcome::Halted
        } else {
            RunOutcome::ReachedTarget
        })
    }

    fn work(&self) -> u64 {
        self.work
    }

    fn state_hash(&self) -> Result<[u8; 32], SubjectError> {
        Ok([self.work as u8; 32])
    }

    fn observable_digest(&self) -> [u8; 32] {
        [if self.work >= self.factory.output_at {
            self.factory.output
        } else {
            0
        }; 32]
    }
}

fn pair(output_at: u64, halt_at: Option<u64>) -> (OutputFactory, OutputFactory) {
    (
        OutputFactory {
            output: b'A',
            output_at,
            halt_at,
        },
        OutputFactory {
            output: b'B',
            output_at,
            halt_at,
        },
    )
}

#[test]
fn output_only_difference_at_spawn_is_a_divergence() {
    let (a, b) = pair(0, None);
    assert_eq!(
        compare_runs(&a, &b, 7, 1, 0).unwrap().verdict,
        Verdict::Diverged {
            last_match: None,
            first_mismatch: 0,
        }
    );
    let point = bisect_divergence(&a, &b, 7, 0, 0).unwrap();
    assert_eq!(point.first_divergent_work, 0);
    assert_eq!(point.hash_a, [0; 32]);
    assert_eq!(point.hash_b, [0; 32]);
    assert_eq!(point.observable_digest_a, [b'A'; 32]);
    assert_eq!(point.observable_digest_b, [b'B'; 32]);
}

#[test]
fn output_only_difference_after_work_is_detected_and_bisected() {
    let (a, b) = pair(3, None);
    assert_eq!(
        compare_runs(&a, &b, 7, 5, 5).unwrap().verdict,
        Verdict::Diverged {
            last_match: None,
            first_mismatch: 5,
        }
    );
    let point = bisect_divergence(&a, &b, 7, 0, 5).unwrap();
    assert_eq!(point.first_divergent_work, 3);
    assert_eq!(point.hash_a, [3; 32]);
    assert_eq!(point.hash_b, [3; 32]);
    assert_eq!(point.observable_digest_a, [b'A'; 32]);
    assert_eq!(point.observable_digest_b, [b'B'; 32]);
}

#[test]
fn output_only_difference_at_equal_halt_count_is_a_divergence() {
    let (a, b) = pair(1, Some(1));
    let report = compare_runs(&a, &b, 7, 5, 5).unwrap();
    assert_eq!(report.halted_at, Some(1));
    assert_eq!(
        report.verdict,
        Verdict::TerminalDiverged {
            last_match: None,
            halted_at: 1,
        }
    );
    assert_eq!(
        bisect_divergence(&a, &b, 7, 0, 1)
            .unwrap()
            .first_divergent_work,
        1,
        "a terminal state reached at the requested work count remains replayable"
    );
}
