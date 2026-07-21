// SPDX-License-Identifier: AGPL-3.0-or-later
//! Determinism test harness and divergence bisector.
//!
//! The project's central invariant is *same seed ⇒ bit-identical execution*.
//! This crate checks that invariant for any [`Subject`] implementation and,
//! when it breaks, finds the exact unit of work at which two supposedly
//! identical runs first diverge: [`compare_runs`] detects and brackets a
//! divergence by hashing state at periodic checkpoints, and
//! [`bisect_divergence`] binary-searches re-executions from scratch to pin
//! down the first divergent work count. "Work" is an abstract monotonic
//! counter (retired branches for the real VM, instructions executed for the
//! bundled [`toy`] machine); all harness logic treats it as opaque ticks. The
//! [`toy`] interpreter plus the divergence-injecting [`flaky`] wrapper let the
//! harness be developed and fully tested before the real VMM exists.

#![warn(missing_docs)]

pub mod flaky;
pub mod toy;

use serde::{Deserialize, Serialize};

/// Errors surfaced by machines and by the harness itself.
///
/// (The spec sketches this as `struct SubjectError(/* String or enum */)`;
/// the enum form is used so callers can distinguish failure classes.)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubjectError {
    /// `run_to` was asked to rewind; machines cannot run backwards.
    #[error("run_to target {target} is behind the current work count {current}")]
    TargetBehind {
        /// The requested target work count.
        target: u64,
        /// The machine's current work count.
        current: u64,
    },
    /// [`compare_runs`] was called with `checkpoint_every == 0`.
    #[error("checkpoint_every must be at least 1")]
    ZeroCheckpointInterval,
    /// [`bisect_divergence`] was called with an empty interval (`lo >= hi`).
    #[error("bisection interval is empty: lo {lo} >= hi {hi}")]
    EmptyInterval {
        /// Lower bound as passed in.
        lo: u64,
        /// Upper bound as passed in.
        hi: u64,
    },
    /// [`bisect_divergence`] found equal state hashes at `hi`: the runs do
    /// not diverge in `(lo, hi]`, so there is no divergence point to report.
    #[error("state hashes are equal at hi = {hi}: no divergence to bisect")]
    NoDivergence {
        /// The upper bound that was probed.
        hi: u64,
    },
    /// [`bisect_divergence`] found differing state hashes at `lo > 0`: the
    /// interval does not bracket the *first* divergence.
    #[error(
        "state hashes already differ at lo = {lo}: interval does not bracket the first divergence"
    )]
    DivergesAtLo {
        /// The lower bound that was probed.
        lo: u64,
    },
}

/// Why a [`Subject::run_to`] call stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// `work() == target` and the machine can continue.
    ReachedTarget,
    /// The machine is halted (possibly at exactly `target`, possibly before).
    Halted,
}

/// A deterministic machine under test. Implementations must guarantee that two
/// instances created with the same seed behave identically — that is the property
/// this harness exists to check.
pub trait Subject {
    /// Run until `work() == target` or the machine halts, whichever first.
    /// `target < work()` is an error (machines cannot run backwards); this is
    /// checked before anything else, even on a halted machine.
    ///
    /// Returns [`RunOutcome::Halted`] whenever the machine is halted on
    /// return — including when it halted exactly at `target` — and
    /// [`RunOutcome::ReachedTarget`] only if `work() == target` and the
    /// machine could continue. Calling `run_to` on a halted machine with
    /// `target >= work()` is a no-op returning `Halted`.
    fn run_to(&mut self, target: u64) -> Result<RunOutcome, SubjectError>;
    /// Current value of the monotonic work counter.
    fn work(&self) -> u64;
    /// Canonical hash of ALL architectural state (registers, memory, output log…).
    /// Must be a pure function of state — calling it twice changes nothing.
    fn state_hash(&self) -> [u8; 32];
    /// Canonical hash of only the **guest-observable output** — the bytes the
    /// guest deliberately emits (serial / output log + event log), carrying
    /// **no** latent device or PRNG state. This is deliberately distinct from
    /// [`Subject::state_hash`], which folds in latent state such as a
    /// seed-derived entropy stream.
    ///
    /// The `acceptance-suite` O3 seed-sensitivity oracle compares this, never
    /// `state_hash`: a payload that consumes RNG without branching on it must
    /// keep an identical work count across seeds while its *observable output*
    /// diverges — yet its `state_hash` would diverge regardless via the latent
    /// seeded PRNG, so the oracle is unsound on `state_hash`.
    ///
    /// The default returns [`Subject::state_hash`] purely for backward
    /// compatibility with machines that predate this accessor; it conflates
    /// observable output with latent state, so **any machine used for O3 must
    /// override it** to hash only its observable output.
    fn observable_digest(&self) -> [u8; 32] {
        self.state_hash()
    }
}

/// Creates fresh machines. Bisection re-executes from scratch many times, so
/// spawning must be cheap and, above all, deterministic. Freshly spawned
/// machines start at `work() == 0`.
pub trait SubjectFactory {
    /// The machine type this factory spawns.
    type M: Subject;
    /// Create a fresh machine whose behaviour is a pure function of `seed`.
    fn spawn(&self, seed: u64) -> Self::M;
}

/// Result of [`compare_runs`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareReport {
    /// What the comparison concluded.
    pub verdict: Verdict,
    /// Number of checkpoints at which both state hashes were compared
    /// (including the mismatching checkpoint of a `Diverged` verdict).
    pub checkpoints_compared: u64,
    /// `Some(w)` iff both machines halted at the same work count `w`.
    /// `None` for `HaltMismatch` (the mismatch carries its own counts) and
    /// whenever the comparison stopped before both machines halted.
    pub halted_at: Option<u64>,
    /// True if the comparison stopped because `limit` was reached rather than
    /// because both machines halted. An `Identical` verdict with `limit_reached`
    /// means "no divergence observed up to limit", NOT "the runs are identical
    /// forever" — callers (and the CLI JSON output) must surface the distinction.
    pub limit_reached: bool,
}

/// Conclusion of a [`compare_runs`] comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// No difference observed (see [`CompareReport::limit_reached`] for
    /// whether this is "proven to halt identically" or only "identical up
    /// to the limit").
    Identical,
    /// Hashes matched at `last_match` and differed at `first_mismatch` — the
    /// divergence lies in (last_match, first_mismatch]. `last_match: None`
    /// means the *first* compared checkpoint already differed; since
    /// [`compare_runs`] never hashes at work 0, that only brackets the
    /// divergence to (0, first_mismatch] — machines that already differ at
    /// spawn land here too (bisection then reports work count 1, the smallest
    /// value in the half-open bracket).
    Diverged {
        /// Last checkpoint at which the hashes still matched; `None` if the
        /// first compared checkpoint already differed.
        last_match: Option<u64>,
        /// First checkpoint at which the hashes differed.
        first_mismatch: u64,
    },
    /// One machine halted at a different work count than the other.
    /// `None` means "had not halted when the mismatch was established".
    HaltMismatch {
        /// Work count at which machine A halted, if it did.
        a: Option<u64>,
        /// Work count at which machine B halted, if it did.
        b: Option<u64>,
    },
}

/// Run a fresh machine from each factory with the same seed, hashing state every
/// `checkpoint_every` work units (and at halt), until both halt or `limit` is reached.
///
/// `checkpoint_every == 0` is an error. `limit == 0` compares nothing and
/// reports `Identical` with `limit_reached: true`. The final checkpoint is
/// clamped to `limit`, so divergence at `limit` itself is still observed.
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
                if ma.state_hash() != mb.state_hash() {
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
                let verdict = if ma.state_hash() != mb.state_hash() {
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
            // One machine halted while the other ran past that halt point
            // without halting — it can never halt there anymore, so the
            // mismatch is already established.
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

/// The exact point where two runs first diverge, found by [`bisect_divergence`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DivergencePoint {
    /// Smallest work count w in (lo, hi] where state hashes differ.
    pub first_divergent_work: u64,
    /// Subject A's state hash at `first_divergent_work` (hex in JSON).
    #[serde(with = "hex32")]
    pub hash_a: [u8; 32],
    /// Subject B's state hash at `first_divergent_work` (hex in JSON).
    #[serde(with = "hex32")]
    pub hash_b: [u8; 32],
    /// Individual machine executions (spawn + run) performed, for the
    /// efficiency gate: at most `2 * (ceil(log2(hi - lo)) + 2)`.
    pub runs_executed: u64,
}

/// Binary-search the exact divergence point, given a bracketing interval from
/// [`compare_runs`]: hashes match at `lo` (or lo == 0), differ at `hi`. Each probe
/// spawns fresh machines and runs to the midpoint — O(log(hi-lo)) probes total.
///
/// Both endpoints are verified before searching: equal hashes at `hi` yield
/// [`SubjectError::NoDivergence`], differing hashes at `lo > 0` yield
/// [`SubjectError::DivergesAtLo`] (`lo == 0` is trusted as the start of time:
/// if the machines already differ at spawn, the smallest divergent work count
/// in `(0, hi]` — i.e. 1 — is reported). The result is the true *first*
/// divergence provided divergence is persistent within the bracket, which
/// holds for any state difference that later execution cannot erase.
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
        Ok((ma.state_hash(), mb.state_hash()))
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
    // Invariant: hashes match at lo (or lo == 0), differ at hi.
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

/// Serde adapter: `[u8; 32]` as a lowercase 64-char hex string.
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
        for (i, pair) in raw.chunks_exact(2).enumerate() {
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

    /// A machine that does NOT override `observable_digest`, so it exercises the
    /// trait's default body (`= state_hash()`).
    struct DefaultDigestMachine {
        hash: [u8; 32],
    }
    impl Subject for DefaultDigestMachine {
        fn run_to(&mut self, _target: u64) -> Result<RunOutcome, SubjectError> {
            Ok(RunOutcome::Halted)
        }
        fn work(&self) -> u64 {
            0
        }
        fn state_hash(&self) -> [u8; 32] {
            self.hash
        }
        // observable_digest deliberately not overridden.
    }

    #[test]
    fn default_observable_digest_is_state_hash_and_varies() {
        // Default body returns state_hash: pin that it equals state_hash (kills a
        // constant `[0; 32]`/`[1; 32]` body) and that it tracks observed state.
        let a = DefaultDigestMachine { hash: [7u8; 32] };
        assert_eq!(a.observable_digest(), a.state_hash());
        assert_eq!(a.observable_digest(), [7u8; 32]);
        let b = DefaultDigestMachine { hash: [9u8; 32] };
        assert_ne!(a.observable_digest(), b.observable_digest());
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
        // Hashes already differ at lo = 10 > 5.
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
