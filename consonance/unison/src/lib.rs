// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod flaky;
pub mod toy;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubjectError {
    #[error("state capture failed: {0}")]
    StateCapture(String),
    #[error("run_to target {target} is behind the current work count {current}")]
    TargetBehind { target: u64, current: u64 },
    #[error("checkpoint_every must be at least 1")]
    ZeroCheckpointInterval,
    #[error("bisection interval is empty: lo {lo} >= hi {hi}")]
    EmptyInterval { lo: u64, hi: u64 },
    #[error("state hashes are equal at hi = {hi}: no divergence to bisect")]
    NoDivergence { hi: u64 },
    #[error(
        "state hashes already differ at lo = {lo}: interval does not bracket the first divergence"
    )]
    DivergesAtLo { lo: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    ReachedTarget,
    Halted,
}

pub trait Subject {
    fn run_to(&mut self, target: u64) -> Result<RunOutcome, SubjectError>;
    fn work(&self) -> u64;
    fn state_hash(&self) -> Result<[u8; 32], SubjectError>;
    fn observable_digest(&self) -> [u8; 32];
}

pub trait SubjectFactory {
    type M: Subject;
    fn spawn(&self, seed: u64) -> Self::M;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareReport {
    pub verdict: Verdict,
    pub checkpoints_compared: u64,
    pub halted_at: Option<u64>,
    pub limit_reached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Identical,
    Diverged {
        last_match: Option<u64>,
        first_mismatch: u64,
    },
    HaltMismatch {
        a: Option<u64>,
        b: Option<u64>,
    },
}

pub fn compare_runs<FA: SubjectFactory, FB: SubjectFactory>(
    a: &FA,
    b: &FB,
    seed: u64,
    checkpoint_every: u64,
    limit: u64,
) -> Result<CompareReport, SubjectError> {
    if checkpoint_every == 0 {
        return Err(SubjectError::ZeroCheckpointInterval);
    }
    let mut ma = a.spawn(seed);
    let mut mb = b.spawn(seed);
    let mut last_match: Option<u64> = None;
    let mut checkpoints_compared = 0u64;
    let mut t = 0u64;
    while t < limit {
        t = t.saturating_add(checkpoint_every).min(limit);
        let oa = ma.run_to(t)?;
        let ob = mb.run_to(t)?;
        let (wa, wb) = (ma.work(), mb.work());
        match (oa, ob) {
            (RunOutcome::ReachedTarget, RunOutcome::ReachedTarget) => {
                checkpoints_compared += 1;
                if ma.state_hash()? != mb.state_hash()? {
                    return Ok(CompareReport {
                        verdict: Verdict::Diverged {
                            last_match,
                            first_mismatch: t,
                        },
                        checkpoints_compared,
                        halted_at: None,
                        limit_reached: false,
                    });
                }
                last_match = Some(t);
            }
            (RunOutcome::Halted, RunOutcome::Halted) => {
                if wa != wb {
                    return Ok(CompareReport {
                        verdict: Verdict::HaltMismatch {
                            a: Some(wa),
                            b: Some(wb),
                        },
                        checkpoints_compared,
                        halted_at: None,
                        limit_reached: false,
                    });
                }
                checkpoints_compared += 1;
                let verdict = if ma.state_hash()? != mb.state_hash()? {
                    Verdict::Diverged {
                        last_match,
                        first_mismatch: wa,
                    }
                } else {
                    Verdict::Identical
                };
                return Ok(CompareReport {
                    verdict,
                    checkpoints_compared,
                    halted_at: Some(wa),
                    limit_reached: false,
                });
            }
            (RunOutcome::Halted, RunOutcome::ReachedTarget) => {
                return Ok(CompareReport {
                    verdict: Verdict::HaltMismatch {
                        a: Some(wa),
                        b: None,
                    },
                    checkpoints_compared,
                    halted_at: None,
                    limit_reached: false,
                });
            }
            (RunOutcome::ReachedTarget, RunOutcome::Halted) => {
                return Ok(CompareReport {
                    verdict: Verdict::HaltMismatch {
                        a: None,
                        b: Some(wb),
                    },
                    checkpoints_compared,
                    halted_at: None,
                    limit_reached: false,
                });
            }
        }
    }
    Ok(CompareReport {
        verdict: Verdict::Identical,
        checkpoints_compared,
        halted_at: None,
        limit_reached: true,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DivergencePoint {
    pub first_divergent_work: u64,
    #[serde(with = "hex32")]
    pub hash_a: [u8; 32],
    #[serde(with = "hex32")]
    pub hash_b: [u8; 32],
    pub runs_executed: u64,
}

pub fn bisect_divergence<FA: SubjectFactory, FB: SubjectFactory>(
    a: &FA,
    b: &FB,
    seed: u64,
    lo: u64,
    hi: u64,
) -> Result<DivergencePoint, SubjectError> {
    if lo >= hi {
        return Err(SubjectError::EmptyInterval { lo, hi });
    }
    let mut runs_executed = 0u64;
    let mut probe = |t: u64| -> Result<([u8; 32], [u8; 32]), SubjectError> {
        let mut ma = a.spawn(seed);
        ma.run_to(t)?;
        runs_executed += 1;
        let mut mb = b.spawn(seed);
        mb.run_to(t)?;
        runs_executed += 1;
        Ok((ma.state_hash()?, mb.state_hash()?))
    };
    let (mut hash_a, mut hash_b) = probe(hi)?;
    if hash_a == hash_b {
        return Err(SubjectError::NoDivergence { hi });
    }
    if lo > 0 {
        let (ha, hb) = probe(lo)?;
        if ha != hb {
            return Err(SubjectError::DivergesAtLo { lo });
        }
    }
    let (mut lo, mut hi) = (lo, hi);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let (ha, hb) = probe(mid)?;
        if ha == hb {
            lo = mid;
        } else {
            hi = mid;
            hash_a = ha;
            hash_b = hb;
        }
    }
    Ok(DivergencePoint {
        first_divergent_work: hi,
        hash_a,
        hash_b,
        runs_executed,
    })
}

mod hex32 {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    const HEX: &[u8; 16] = b"0123456789abcdef";

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], ser: S) -> Result<S::Ok, S::Error> {
        let mut s = String::with_capacity(64);
        for b in bytes {
            s.push(char::from(HEX[usize::from(b >> 4)]));
            s.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
        ser.serialize_str(&s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<[u8; 32], D::Error> {
        fn nibble(c: u8) -> Option<u8> {
            match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                b'A'..=b'F' => Some(c - b'A' + 10),
                _ => None,
            }
        }
        let s = String::deserialize(de)?;
        let raw = s.as_bytes();
        if raw.len() != 64 {
            return Err(D::Error::custom("expected 64 hex characters"));
        }
        let mut out = [0u8; 32];
        let (pairs, remainder) = raw.as_chunks::<2>();
        debug_assert!(remainder.is_empty());
        for (i, pair) in pairs.iter().enumerate() {
            let hi = nibble(pair[0]).ok_or_else(|| D::Error::custom("invalid hex digit"))?;
            let lo = nibble(pair[1]).ok_or_else(|| D::Error::custom("invalid hex digit"))?;
            out[i] = (hi << 4) | lo;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flaky::{FlakyFactory, Perturbation};
    use crate::toy::{ToyFactory, generate_program};

    fn factory() -> ToyFactory {
        ToyFactory {
            program: generate_program(1, 500).instrs,
        }
    }

    struct WhollyObservableMachine {
        hash: [u8; 32],
    }
    impl Subject for WhollyObservableMachine {
        fn run_to(&mut self, _target: u64) -> Result<RunOutcome, SubjectError> {
            Ok(RunOutcome::Halted)
        }
        fn work(&self) -> u64 {
            0
        }
        fn state_hash(&self) -> Result<[u8; 32], SubjectError> {
            Ok(self.hash)
        }
        fn observable_digest(&self) -> [u8; 32] {
            self.hash
        }
    }

    #[test]
    fn a_wholly_observable_machine_states_that_its_digests_coincide() {
        let a = WhollyObservableMachine { hash: [7u8; 32] };
        assert_eq!(a.observable_digest(), a.state_hash().unwrap());
        assert_eq!(a.observable_digest(), [7u8; 32]);
        let b = WhollyObservableMachine { hash: [9u8; 32] };
        assert_ne!(a.observable_digest(), b.observable_digest());
    }

    struct FailingCapture;
    impl Subject for FailingCapture {
        fn run_to(&mut self, _: u64) -> Result<RunOutcome, SubjectError> {
            Ok(RunOutcome::Halted)
        }
        fn work(&self) -> u64 {
            0
        }
        fn state_hash(&self) -> Result<[u8; 32], SubjectError> {
            Err(SubjectError::StateCapture(
                "extension capture failed".into(),
            ))
        }
        fn observable_digest(&self) -> [u8; 32] {
            [0; 32]
        }
    }
    impl SubjectFactory for FailingCapture {
        type M = Self;
        fn spawn(&self, _: u64) -> Self {
            Self
        }
    }
    #[test]
    fn identical_capture_failures_are_not_identical_executions() {
        assert_eq!(
            compare_runs(&FailingCapture, &FailingCapture, 0, 1, 1),
            Err(SubjectError::StateCapture(
                "extension capture failed".into()
            ))
        );
    }

    #[test]
    fn zero_checkpoint_interval_is_an_error() {
        let f = factory();
        assert_eq!(
            compare_runs(&f, &f, 3, 0, 100),
            Err(SubjectError::ZeroCheckpointInterval)
        );
    }

    #[test]
    fn zero_limit_compares_nothing() {
        let f = factory();
        let r = compare_runs(&f, &f, 3, 10, 0).unwrap();
        assert_eq!(r.verdict, Verdict::Identical);
        assert_eq!(r.checkpoints_compared, 0);
        assert_eq!(r.halted_at, None);
        assert!(r.limit_reached);
    }

    #[test]
    fn empty_interval_is_an_error() {
        let f = factory();
        assert_eq!(
            bisect_divergence(&f, &f, 3, 7, 7),
            Err(SubjectError::EmptyInterval { lo: 7, hi: 7 })
        );
        assert_eq!(
            bisect_divergence(&f, &f, 3, 8, 7),
            Err(SubjectError::EmptyInterval { lo: 8, hi: 7 })
        );
    }

    #[test]
    fn non_divergent_pair_is_a_documented_error() {
        let f = factory();
        assert_eq!(
            bisect_divergence(&f, &f, 3, 0, 100),
            Err(SubjectError::NoDivergence { hi: 100 })
        );
    }

    #[test]
    fn bad_bracket_lo_is_an_error() {
        let f = factory();
        let flaky = FlakyFactory {
            inner: factory(),
            diverge_at: 5,
            perturb: Perturbation::XorPrng { mask: 0xABCD },
        };
        assert_eq!(
            bisect_divergence(&f, &flaky, 3, 10, 20),
            Err(SubjectError::DivergesAtLo { lo: 10 })
        );
    }

    #[test]
    fn divergence_point_hex_json_round_trips() {
        let p = DivergencePoint {
            first_divergent_work: 42,
            hash_a: [0xAB; 32],
            hash_b: [0x01; 32],
            runs_executed: 12,
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains(&"ab".repeat(32)));
        let back: DivergencePoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }
}
