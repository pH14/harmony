// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`RegimeTactic`] — the spine [`Tactic`] that answers surfaced fault decisions
//! from the bursty [`RegimeProcess`]. Open-loop by construction: its answer is a
//! function of `(own regime state, point, rng)` and nothing else (crate
//! invariant (a)) — no `Sensor`/`Archive`/`RunTrace` type appears in its inputs
//! or this crate's dependency graph.

use environment::{Answer as EnvAnswer, DecisionClass};
use explorer::{Answer, DecisionPoint, Prng, Tactic};

use crate::regime::{RegimeParams, RegimeProcess};

/// The stable wire discriminant of a guest [`DecisionClass`], matching
/// `environment`'s `#[repr(u16)]` numbering (defined locally — `environment`
/// keeps its `as_u16`/`from_u16` crate-private). This crate's `ctx` convention
/// (see [`class_of`]) reads this tag from the leading bytes of a decision point.
pub fn class_tag(class: DecisionClass) -> u16 {
    match class {
        DecisionClass::Entropy => 1,
        DecisionClass::Payload => 2,
        DecisionClass::Scheduler => 3,
        DecisionClass::NetFlow => 4,
        DecisionClass::BlockIo => 5,
        DecisionClass::Process => 6,
    }
}

/// Decode a class tag; the inverse of [`class_tag`], rejecting unknown values.
fn class_from_tag(v: u16) -> Option<DecisionClass> {
    Some(match v {
        1 => DecisionClass::Entropy,
        2 => DecisionClass::Payload,
        3 => DecisionClass::Scheduler,
        4 => DecisionClass::NetFlow,
        5 => DecisionClass::BlockIo,
        6 => DecisionClass::Process,
        _ => return None,
    })
}

/// The `ctx` convention this tactic reads: a surfaced [`DecisionPoint`]'s opaque
/// `ctx` bytes **begin** with the little-endian [`class_tag`] of the decision's
/// class. `None` when `ctx` is too short or carries an unknown tag — in which
/// case the tactic declines (falls through to the seed), never fabricating an
/// answer for a class it cannot identify. Exposed so a campaign/machine and the
/// gates can stamp points the tactic understands.
pub fn class_of(ctx: &[u8]) -> Option<DecisionClass> {
    let tag = u16::from_le_bytes([*ctx.first()?, *ctx.get(1)?]);
    class_from_tag(tag)
}

/// The bursty regime [`Tactic`]. Answers **fault-class** decisions from the
/// modulated regime and lets **supply**-class (and unidentifiable) decisions
/// fall through to the seeded base — the regime governs the fault classes only.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RegimeTactic {
    regime: RegimeProcess,
}

impl RegimeTactic {
    /// Build a tactic over an explicit regime.
    pub fn new(params: RegimeParams) -> Self {
        Self {
            regime: RegimeProcess::new(params),
        }
    }

    /// Build a tactic whose regime is drawn once from `seed` (the per-run
    /// swizzle draw).
    pub fn from_seed(seed: u64) -> Self {
        Self {
            regime: RegimeProcess::from_seed(seed),
        }
    }

    /// The underlying regime chain (for inspection/gates).
    pub fn regime(&self) -> &RegimeProcess {
        &self.regime
    }
}

impl Tactic for RegimeTactic {
    /// For a fault-class decision: advance the regime one step, then sample the
    /// active state's table for the point's class, emitting the encoded guest
    /// [`Answer`](environment::Answer). For a supply class or an unidentifiable
    /// `ctx`: decline with the empty answer, so the seeded base supplies the
    /// value nominally and no override is fabricated. The regime advances only on
    /// governed (fault-class) decisions, so every step corresponds to a fault
    /// draw — keeping the stationary-rate accounting exact.
    fn decide(&mut self, pt: &DecisionPoint, rng: &mut Prng) -> Answer {
        match class_of(&pt.ctx) {
            Some(class) if class.is_fault() => {
                self.regime.step(rng);
                let ans: EnvAnswer = self.regime.sample(class, rng);
                Answer(ans.encode())
            }
            // Supply class, or an unidentifiable point: decline to the seed.
            _ => Answer(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regime::{Regime, StateTable};

    /// A `ctx` prefixed with a class tag decodes back to that class; a short or
    /// unknown `ctx` decodes to `None`.
    #[test]
    fn class_ctx_round_trips() {
        for class in [
            DecisionClass::Entropy,
            DecisionClass::Payload,
            DecisionClass::Scheduler,
            DecisionClass::NetFlow,
            DecisionClass::BlockIo,
            DecisionClass::Process,
        ] {
            let ctx = class_tag(class).to_le_bytes().to_vec();
            assert_eq!(class_of(&ctx), Some(class));
        }
        assert_eq!(class_of(&[]), None);
        assert_eq!(class_of(&[7]), None, "one byte is too short");
        assert_eq!(class_of(&99u16.to_le_bytes()), None, "unknown tag");
    }

    /// Helper: a decision point carrying `class` in its `ctx`.
    fn point(class: DecisionClass, id: u64) -> DecisionPoint {
        DecisionPoint {
            at: explorer::Moment(id),
            id,
            ctx: class_tag(class).to_le_bytes().to_vec(),
        }
    }

    /// A supply-class decision always declines (empty answer), whatever the
    /// regime state.
    #[test]
    fn supply_decision_declines() {
        let mut t = RegimeTactic::from_seed(3);
        let mut rng = Prng::new(1);
        let a = t.decide(&point(DecisionClass::Entropy, 0), &mut rng);
        assert_eq!(a, Answer(Vec::new()));
    }

    /// An unidentifiable ctx declines and draws no PRNG (the stream is
    /// untouched), so it can never desync a replay.
    #[test]
    fn unknown_ctx_declines_without_drawing() {
        let mut t = RegimeTactic::from_seed(3);
        let mut rng = Prng::new(1);
        let snapshot = rng.clone();
        let pt = DecisionPoint {
            at: explorer::Moment(0),
            id: 0,
            ctx: vec![0xEE], // one byte — unidentifiable
        };
        assert_eq!(t.decide(&pt, &mut rng), Answer(Vec::new()));
        assert_eq!(rng, snapshot, "declining an unknown point draws nothing");
    }

    /// A storm-forced fault-class decision emits a decodable in-class guest
    /// fault; a calm-forced one emits nominal.
    #[test]
    fn fault_decision_emits_encoded_answer() {
        // Storm at 1/1, calm at 0/1; force the regime to storm and never leave.
        let params = RegimeParams::new(
            (1, 1),
            (0, 1),
            StateTable::uniform(0, crate::regime::DEN_CAP).unwrap(),
            StateTable::uniform(crate::regime::DEN_CAP, crate::regime::DEN_CAP).unwrap(),
        )
        .unwrap();
        let mut t = RegimeTactic::new(params);
        let mut rng = Prng::new(5);
        // First decision steps Calm->Storm (1/1), then samples storm (1/1 fault).
        let a = t.decide(&point(DecisionClass::BlockIo, 1), &mut rng);
        assert_eq!(t.regime().state(), Regime::Storm);
        let decoded = EnvAnswer::decode(&a.0).expect("decodable answer");
        match decoded {
            EnvAnswer::Fault(f) => assert_eq!(f.class(), DecisionClass::BlockIo),
            other => panic!("expected a block fault, got {other:?}"),
        }
    }

    /// Same seed + same point sequence + same rng seed ⇒ identical answer
    /// sequence (statistical gate (a), determinism).
    #[test]
    fn same_seed_same_sequence() {
        let pts: Vec<_> = (0..64).map(|i| point(DecisionClass::NetFlow, i)).collect();
        let run = || {
            let mut t = RegimeTactic::from_seed(77);
            let mut rng = Prng::new(1234);
            pts.iter()
                .map(|p| t.decide(p, &mut rng))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }
}
