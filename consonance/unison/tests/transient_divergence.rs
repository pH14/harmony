// SPDX-License-Identifier: AGPL-3.0-or-later

use unison::{
    RunOutcome, Subject, SubjectError, SubjectFactory, Verdict, bisect_divergence, compare_runs,
    first_divergence_from_report,
};

#[derive(Clone, Copy)]
struct Factory(bool);

struct Machine {
    work: u64,
    divergent: bool,
}

impl SubjectFactory for Factory {
    type M = Machine;

    fn spawn(&self, _: u64) -> Self::M {
        Machine {
            work: 0,
            divergent: self.0,
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
        self.work = target;
        Ok(RunOutcome::ReachedTarget)
    }

    fn work(&self) -> u64 {
        self.work
    }

    fn state_hash(&self) -> Result<[u8; 32], SubjectError> {
        let mut hash = [self.work as u8; 32];
        if self.divergent && (self.work == 1 || self.work >= 5) {
            hash[0] ^= 0x80;
        }
        Ok(hash)
    }

    fn observable_digest(&self) -> [u8; 32] {
        [0; 32]
    }
}

#[test]
fn first_mismatch_precedes_a_matching_checkpoint() {
    let a = Factory(false);
    let b = Factory(true);
    let report = compare_runs(&a, &b, 7, 5, 5).unwrap();
    assert_eq!(
        report.verdict,
        Verdict::Diverged {
            last_match: None,
            first_mismatch: 5,
        }
    );
    let point = bisect_divergence(&a, &b, 7, 0, 5).unwrap();
    assert_eq!(point.first_divergent_work, 1);
    assert_ne!(point.hash_a, point.hash_b);
}

#[test]
fn a_later_matching_checkpoint_does_not_prove_no_earlier_difference() {
    let a = Factory(false);
    let b = Factory(true);
    let report = compare_runs(&a, &b, 7, 4, 8).unwrap();
    assert_eq!(
        report.verdict,
        Verdict::Diverged {
            last_match: Some(4),
            first_mismatch: 8,
        }
    );
    let bracket = bisect_divergence(&a, &b, 7, 4, 8).unwrap();
    assert_eq!(bracket.first_divergent_work, 5);
    let from_start = bisect_divergence(&a, &b, 7, 0, 8).unwrap();
    assert_eq!(from_start.first_divergent_work, 1);
    let reported = first_divergence_from_report(&a, &b, 7, &report)
        .unwrap()
        .unwrap();
    assert_eq!(reported.first_divergent_work, 1);
}

#[test]
fn matching_sampled_checkpoints_do_not_claim_global_identity() {
    let a = Factory(false);
    let b = Factory(true);
    let report = compare_runs(&a, &b, 7, 4, 4).unwrap();
    assert_eq!(report.checkpoints_compared, 1);
    assert!(report.limit_reached);
    assert_eq!(
        serde_json::to_value(&report).unwrap()["verdict"],
        serde_json::json!("matching_checkpoints")
    );
    assert_eq!(
        bisect_divergence(&a, &b, 7, 0, 1)
            .unwrap()
            .first_divergent_work,
        1
    );
}
