// SPDX-License-Identifier: AGPL-3.0-or-later
//! The determinism oracles (O1–O3) and their per-oracle results.
//!
//! Each oracle is a thin domain wrapper over `unison`'s generic primitives,
//! written against `unison::MachineFactory` so it is fully testable now with the
//! toy machine and pointed at the real `Vmm` at integration with no API change.

use serde::{Serialize, Serializer};
use unison::{
    DivergencePoint, Machine, MachineError, MachineFactory, RunOutcome, Verdict, bisect_divergence,
    compare_runs,
};

/// Which oracle a corpus item participates in. See `docs/DETERMINISM-CORPUS.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Oracle {
    /// O1: two runs at the same seed are bit-identical (localized on failure).
    Determinism,
    /// O2: the guest-observable output digest (`observable_digest`) equals a
    /// committed golden.
    Conformance,
    /// O3: behaviour under two *different* seeds is non-trivial in the declared
    /// way (`rng_consuming` selects which non-triviality is asserted).
    SeedSensitivity {
        /// True iff the payload deliberately consumes the RNG stream.
        rng_consuming: bool,
    },
}

impl Oracle {
    /// Stable token used in both the manifest and the JSON report.
    pub(crate) fn to_token(self) -> &'static str {
        match self {
            Oracle::Determinism => "determinism",
            Oracle::Conformance => "conformance",
            Oracle::SeedSensitivity {
                rng_consuming: true,
            } => "seed_sensitivity:rng",
            Oracle::SeedSensitivity {
                rng_consuming: false,
            } => "seed_sensitivity:pure",
        }
    }

    /// Parse a token. `None` for an unrecognized token.
    pub(crate) fn from_token(s: &str) -> Option<Oracle> {
        match s {
            "determinism" => Some(Oracle::Determinism),
            "conformance" => Some(Oracle::Conformance),
            "seed_sensitivity:rng" => Some(Oracle::SeedSensitivity {
                rng_consuming: true,
            }),
            "seed_sensitivity:pure" => Some(Oracle::SeedSensitivity {
                rng_consuming: false,
            }),
            _ => None,
        }
    }
}

impl Serialize for Oracle {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(self.to_token())
    }
}

/// The outcome of running one oracle against one item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OracleResult {
    /// Which oracle produced this result.
    pub oracle: Oracle,
    /// Whether the property held.
    pub passed: bool,
    /// Set on an O1 failure: the exact divergence point from the bisector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub divergence: Option<DivergencePoint>,
    /// Human-readable detail (golden mismatch summary, seed-sensitivity
    /// violation, divergence work count, …).
    pub detail: String,
}

/// O1 — Determinism. Run the same factory twice at `seed`, hashing state every
/// `checkpoint_every` work units up to `limit`; on divergence, bisect and attach
/// the exact first-divergent work count.
///
/// A divergence detected by `compare_runs` is always reported as `passed:
/// false`; if the bisector cannot localize it (e.g. the divergence is not
/// reproducible across re-execution) the result still fails, with the reason in
/// `detail` and `divergence: None`.
pub fn check_determinism<F: MachineFactory>(
    f: &F,
    seed: u64,
    checkpoint_every: u64,
    limit: u64,
) -> Result<OracleResult, MachineError> {
    let report = compare_runs(f, f, seed, checkpoint_every, limit)?;
    let oracle = Oracle::Determinism;
    // No checkpoint was ever compared (e.g. `limit == 0`): the runs were never
    // observed, so "Identical" here is vacuous — report inconclusive, never green.
    if report.verdict == Verdict::Identical && report.checkpoints_compared == 0 {
        return Ok(OracleResult {
            oracle,
            passed: false,
            divergence: None,
            detail: format!(
                "inconclusive: 0 checkpoints compared (limit {limit} verified nothing)"
            ),
        });
    }
    match report.verdict {
        Verdict::Identical => {
            let detail = if report.limit_reached {
                format!(
                    "identical across {} checkpoints up to limit {limit} (not proven beyond limit)",
                    report.checkpoints_compared
                )
            } else {
                format!(
                    "identical; both halted at {}",
                    report
                        .halted_at
                        .map_or_else(|| "?".to_string(), |w| w.to_string())
                )
            };
            Ok(OracleResult {
                oracle,
                passed: true,
                divergence: None,
                detail,
            })
        }
        Verdict::Diverged {
            last_match,
            first_mismatch,
        } => {
            let lo = last_match.unwrap_or(0);
            match bisect_divergence(f, f, seed, lo, first_mismatch) {
                Ok(point) => {
                    let detail = format!(
                        "diverged: first divergent work = {}",
                        point.first_divergent_work
                    );
                    Ok(OracleResult {
                        oracle,
                        passed: false,
                        divergence: Some(point),
                        detail,
                    })
                }
                Err(e) => Ok(OracleResult {
                    oracle,
                    passed: false,
                    divergence: None,
                    detail: format!(
                        "diverged in ({lo}, {first_mismatch}] but bisection could not localize it: {e}"
                    ),
                }),
            }
        }
        Verdict::HaltMismatch { a, b } => Ok(OracleResult {
            oracle,
            passed: false,
            divergence: None,
            detail: format!(
                "halt mismatch: run A halted at {}, run B at {}",
                a.map_or_else(|| "never".to_string(), |w| w.to_string()),
                b.map_or_else(|| "never".to_string(), |w| w.to_string()),
            ),
        }),
    }
}

/// O2 — Conformance. Run once at `seed` to `limit`, compare the final
/// **`observable_digest`** (hex) to `golden_hex`. A malformed or short `golden_hex`
/// is a `Fail` with a detail, never a panic.
///
/// O2 pins the **guest-observable conformance output** — the bytes the guest
/// deliberately emits — exactly as O3 does (`docs/DETERMINISM-CORPUS.md`), not the
/// full `state_hash` (which also folds latent, seed-derived state that is brittle to
/// pin as a golden). For a machine that does not override
/// [`Machine::observable_digest`] it equals `state_hash` (the trait default), so the
/// toy goldens are unaffected; for the VMM corpus bridge it is the report-stream
/// digest, which is what the committed `guest/golden/*.digest` O2 goldens carry — so
/// the generic runner and the box `box_corpus` gate compare the **same** quantity.
pub fn check_conformance<F: MachineFactory>(
    f: &F,
    seed: u64,
    limit: u64,
    golden_hex: &str,
) -> Result<OracleResult, MachineError> {
    let mut m = f.spawn(seed);
    m.run_to(limit)?;
    let actual = m.observable_digest();
    let actual_hex = encode_hex32(&actual);
    let (passed, detail) = match decode_hex32(golden_hex) {
        None => (
            false,
            format!(
                "golden is not 64 hex chars (got {:?}); observed observable_digest = {actual_hex}",
                golden_hex.trim()
            ),
        ),
        Some(golden) if golden == actual => (
            true,
            format!("observable_digest matches golden {actual_hex}"),
        ),
        Some(_) => (
            false,
            format!(
                "observable_digest mismatch: observed {actual_hex} != golden {}",
                golden_hex.trim()
            ),
        ),
    };
    Ok(OracleResult {
        oracle: Oracle::Conformance,
        passed,
        divergence: None,
        detail,
    })
}

/// O3 — Seed-sensitivity (anti-cheat). Run at `seed_a` and `seed_b` (which must
/// differ) to `limit`, comparing the guest-**observable output** digest
/// (`observable_digest`), **not** `state_hash`.
///
/// - `rng_consuming`: assert `work_a == work_b` (control flow is seed-stable)
///   **and** `out_a != out_b` (the seed actually reached observable output). A
///   faked-deterministic RNG (wired to a constant) fails the second clause.
/// - otherwise: assert `out_a == out_b` (nothing seed-dependent reached output).
///   A factory that leaks the seed into observable state fails this.
///
/// **Both runs must halt within `limit`.** If either is still running at `limit`,
/// the comparison is over a bounded prefix, not terminal behaviour: `work_a ==
/// work_b` would then be true merely because both were capped at `limit` (an
/// RNG payload that branches into two long-running paths would pass vacuously).
/// A non-terminating run is reported as a `Fail` ("inconclusive"), never a pass.
///
/// Equal seeds make the oracle meaningless; that is reported as a `Fail` (with a
/// detail), never an error or panic. (The caller is responsible for passing
/// seeds whose *effective* machine state differs — e.g. the toy registry's seed
/// 0 normalizes to `ZERO_SEED_STATE`; the binary accounts for that.)
pub fn check_seed_sensitivity<F: MachineFactory>(
    f: &F,
    seed_a: u64,
    seed_b: u64,
    limit: u64,
    rng_consuming: bool,
) -> Result<OracleResult, MachineError> {
    let oracle = Oracle::SeedSensitivity { rng_consuming };
    if seed_a == seed_b {
        return Ok(OracleResult {
            oracle,
            passed: false,
            divergence: None,
            detail: format!("seed_a == seed_b ({seed_a}); O3 requires two distinct seeds"),
        });
    }
    let mut ma = f.spawn(seed_a);
    let halted_a = matches!(ma.run_to(limit)?, RunOutcome::Halted);
    let mut mb = f.spawn(seed_b);
    let halted_b = matches!(mb.run_to(limit)?, RunOutcome::Halted);
    let (work_a, work_b) = (ma.work(), mb.work());

    // Terminal comparison only: a run still going at `limit` makes work equality
    // an artifact of the cap, not of seed-stable control flow.
    if !halted_a || !halted_b {
        return Ok(OracleResult {
            oracle,
            passed: false,
            divergence: None,
            detail: format!(
                "inconclusive: run did not halt within limit {limit} \
                 (a: halted={halted_a} at work {work_a}, b: halted={halted_b} at work {work_b}); \
                 O3 needs terminal runs, not a bounded prefix"
            ),
        });
    }

    let out_eq = ma.observable_digest() == mb.observable_digest();

    let (passed, detail) = if rng_consuming {
        let work_eq = work_a == work_b;
        match (work_eq, out_eq) {
            (true, false) => (
                true,
                format!("seed-stable work {work_a}, output varied with the seed (good)"),
            ),
            (false, _) => (
                false,
                format!(
                    "control flow is seed-dependent: work_a {work_a} != work_b {work_b} \
                     (RNG-consuming payloads must keep an identical work count across seeds)"
                ),
            ),
            (true, true) => (
                false,
                format!(
                    "faked determinism: work matched ({work_a}) but observable output did NOT \
                     change with the seed (the RNG is not actually reaching output)"
                ),
            ),
        }
    } else if out_eq {
        (
            true,
            "observable output is identical across seeds (good — payload is seed-pure)".to_string(),
        )
    } else {
        (
            false,
            "seed leak: a payload declared seed-pure produced different observable output \
             across seeds"
                .to_string(),
        )
    };

    Ok(OracleResult {
        oracle,
        passed,
        divergence: None,
        detail,
    })
}

/// Lowercase 64-char hex of a 32-byte digest.
fn encode_hex32(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for &b in bytes {
        s.push(char::from(HEX[usize::from(b >> 4)]));
        s.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    s
}

/// Parse exactly 64 hex chars (leading/trailing whitespace trimmed) into a
/// 32-byte digest. `None` on any malformed input — total, never panics.
fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    let raw = s.trim().as_bytes();
    if raw.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in raw.chunks_exact(2).enumerate() {
        // High nibble (bits 4-7) + low nibble (bits 0-3): the operands never
        // share a bit, so `+` here equals `|`/`^` — but `+` keeps the byte
        // assembly distinguishable under mutation (a `^`/`&` swap on `|` would be
        // an undetectable equivalent mutant on non-overlapping nibbles).
        out[i] = (hex_nibble(pair[0])? << 4) + hex_nibble(pair[1])?;
    }
    Some(out)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_rejects_garbage() {
        let bytes = [0xABu8; 32];
        let hex = encode_hex32(&bytes);
        assert_eq!(hex, "ab".repeat(32));
        assert_eq!(decode_hex32(&hex), Some(bytes));
        assert_eq!(decode_hex32("  ".repeat(0).as_str()), None); // empty
        assert_eq!(decode_hex32("zz"), None); // wrong len + non-hex
        assert_eq!(decode_hex32(&"a".repeat(63)), None); // odd/short
        assert_eq!(decode_hex32(&"g".repeat(64)), None); // right len, non-hex
        // Trimming + uppercase accepted.
        assert_eq!(
            decode_hex32(&format!("  {}  ", "AB".repeat(32))),
            Some(bytes)
        );
    }

    #[test]
    fn decode_hex32_assembles_nibbles_exactly() {
        // Distinct high/low nibbles per byte, incl. a zero high nibble — pins the
        // (hi << 4) + lo byte assembly against any operator mutation.
        let hex = "0a1bfe70".to_string() + &"00".repeat(28);
        let got = decode_hex32(&hex).unwrap();
        assert_eq!(&got[..4], &[0x0a, 0x1b, 0xfe, 0x70]);
    }

    /// A machine whose `run_to` jumps work straight to the target and reports
    /// `Halted` only for the configured seed; `observable_digest` varies by seed.
    struct OneHalt {
        halts: bool,
        work: u64,
        out: [u8; 32],
    }
    impl Machine for OneHalt {
        fn run_to(&mut self, target: u64) -> Result<RunOutcome, MachineError> {
            if target < self.work {
                return Err(MachineError::TargetBehind {
                    target,
                    current: self.work,
                });
            }
            self.work = target;
            Ok(if self.halts {
                RunOutcome::Halted
            } else {
                RunOutcome::ReachedTarget
            })
        }
        fn work(&self) -> u64 {
            self.work
        }
        fn state_hash(&self) -> [u8; 32] {
            self.out
        }
        fn observable_digest(&self) -> [u8; 32] {
            self.out
        }
    }
    struct OneHaltFactory {
        halt_seed: u64,
    }
    impl MachineFactory for OneHaltFactory {
        type M = OneHalt;
        fn spawn(&self, seed: u64) -> OneHalt {
            OneHalt {
                halts: seed == self.halt_seed,
                work: 0,
                out: [seed as u8; 32],
            }
        }
    }

    #[test]
    fn one_run_not_halting_is_inconclusive_even_when_work_matches() {
        // seed 1 halts, seed 2 runs to the cap: both reach work == limit (so the
        // work-equality clause would PASS), but exactly one halted. The guard must
        // short-circuit to inconclusive — this kills `||` -> `&&` (which would
        // require BOTH to be non-halting before flagging it).
        let f = OneHaltFactory { halt_seed: 1 };
        let res = check_seed_sensitivity(&f, 1, 2, 100, true).unwrap();
        assert!(
            !res.passed,
            "one non-halting run must be inconclusive, not a pass: {res:?}"
        );
        assert!(res.detail.contains("inconclusive"), "{}", res.detail);
        // Symmetric: the other run is the non-halting one.
        let res =
            check_seed_sensitivity(&OneHaltFactory { halt_seed: 2 }, 1, 2, 100, true).unwrap();
        assert!(!res.passed, "{res:?}");
    }

    #[test]
    fn zero_limit_determinism_is_inconclusive_not_green() {
        use unison::toy::{ToyFactory, generate_program};
        let f = ToyFactory {
            program: generate_program(1, 200).instrs,
        };
        // limit 0 ⇒ compare_runs compares nothing ⇒ must NOT report passed.
        let res = check_determinism(&f, 7, 64, 0).unwrap();
        assert!(!res.passed, "{res:?}");
        assert!(res.detail.contains("inconclusive"), "{}", res.detail);
    }

    #[test]
    fn oracle_token_round_trip() {
        for o in [
            Oracle::Determinism,
            Oracle::Conformance,
            Oracle::SeedSensitivity {
                rng_consuming: true,
            },
            Oracle::SeedSensitivity {
                rng_consuming: false,
            },
        ] {
            assert_eq!(Oracle::from_token(o.to_token()), Some(o));
        }
        assert_eq!(Oracle::from_token("nonsense"), None);
    }
}
