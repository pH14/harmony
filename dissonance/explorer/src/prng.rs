// SPDX-License-Identifier: AGPL-3.0-or-later
//! The local xorshift64\* generator every seeded policy draw comes from.
//!
//! Re-implemented locally (conventions rule 2 — no sibling dependency) and
//! identical to the `hypercall-proto`/`environment` deterministic-entropy
//! algorithm, so a policy's choices are portable and golden-stable. xorshift64\*
//! is a bijection on the nonzero state space, so a normalized seed never collapses
//! the stream to zero. This is the *only* source of "randomness" in the engine —
//! there is no `rand`, no wall-clock, no host entropy (conventions rule 4).
//!
//! Public since task 64: [`Tactic::decide`](crate::Tactic::decide) and
//! [`Selector::choose`](crate::Selector::choose) receive the campaign stream as
//! `&mut Prng`, so plugin crates need the type. `Clone` deliberately exposes the
//! state for record/replay (the open-loop proptest snapshots the stream at each
//! decision and replays it standalone).

use serde::{Deserialize, Deserializer, Serialize};

/// xorshift64\* multiplier (the `hypercall-proto` constant).
const MUL: u64 = 0x2545_F491_4F6C_DD1D;
/// Seed substituted for a zero seed, so the nonzero-state invariant holds (the
/// golden-ratio constant, matching `hypercall-proto`'s fallback).
const FALLBACK: u64 = 0x9E37_79B9_7F4A_7C15;

/// A deterministic xorshift64\* stream. Each [`next_u64`](Prng::next_u64) both
/// advances the state and returns the scrambled output word.
///
/// `Deserialize` is hand-written (not derived): xorshift64\* has one absorbing
/// state — zero, from which every draw is zero forever — and [`Prng::new`]
/// makes it unreachable. A derived impl would let an untrusted `{"state":0}`
/// blob restore exactly the state serialization can never produce (the
/// restore-path rule), so deserialization funnels through `new`, which remaps
/// zero to the fallback just as seeding does.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Prng {
    state: u64,
}

impl<'de> Deserialize<'de> for Prng {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        /// The derived wire shape, so serialize/deserialize stay symmetric.
        #[derive(Deserialize)]
        struct Raw {
            state: u64,
        }
        // Funnel through `new`: the zero state is unrepresentable, exactly as
        // it is for a freshly-seeded stream.
        Ok(Prng::new(Raw::deserialize(d)?.state))
    }
}

impl Prng {
    /// Start a stream from `seed` (zero is remapped to a fixed nonzero seed).
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { FALLBACK } else { seed },
        }
    }

    /// Advance the stream and return the next 64-bit output.
    pub fn next_u64(&mut self) -> u64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(MUL)
    }
}

#[cfg(test)]
mod tests {
    use super::{FALLBACK, Prng};

    /// A pinned golden sequence — the exact `xorshift64*` output for `seed = 1`.
    /// Any change to the shift amounts, directions, the `^=` folds, or the
    /// multiply changes these words, so this locks the algorithm bit-for-bit.
    #[test]
    fn golden_sequence_for_seed_one() {
        let mut p = Prng::new(1);
        assert_eq!(p.next_u64(), 0x47E4_CE4B_896C_DD1D);
        assert_eq!(p.next_u64(), 0xABCF_A6A8_E079_651D);
        assert_eq!(p.next_u64(), 0xB9D1_0D8F_EB73_1F57);
        assert_eq!(p.next_u64(), 0x4DB4_18A0_BB1B_019D);
    }

    /// A zero seed remaps to the fixed golden-ratio fallback, and yields exactly
    /// the fallback's stream.
    #[test]
    fn zero_seed_remaps_to_fallback() {
        let mut z = Prng::new(0);
        assert_eq!(z.next_u64(), 0x0D83_B3E2_9A21_487A);
        assert_eq!(z.next_u64(), 0x54C4_4C79_F1FE_9D67);

        let mut from_zero = Prng::new(0);
        let mut from_fallback = Prng::new(FALLBACK);
        assert_eq!(from_zero.next_u64(), from_fallback.next_u64());
    }

    /// Cloning snapshots the stream state: the clone and the original produce
    /// the same continuation (the record/replay property the open-loop gate
    /// leans on).
    #[test]
    fn clone_snapshots_the_stream() {
        let mut p = Prng::new(42);
        p.next_u64();
        let mut q = p.clone();
        assert_eq!(p.next_u64(), q.next_u64());
    }

    /// A mid-stream serde round-trip preserves the continuation exactly.
    #[test]
    fn serde_round_trip_preserves_the_stream() {
        let mut p = Prng::new(42);
        p.next_u64();
        let json = serde_json::to_string(&p).expect("serialize");
        let mut q: Prng = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, q);
        assert_eq!(p.next_u64(), q.next_u64());
    }

    /// The zero payload — a state serialization can never produce (xorshift's
    /// absorbing state, unreachable via `new`) — deserializes to the same
    /// stream as `Prng::new(0)`, never to the all-zero stream.
    #[test]
    fn zero_payload_cannot_restore_the_absorbing_state() {
        let mut z: Prng = serde_json::from_str(r#"{"state":0}"#).expect("deserialize");
        let mut seeded_zero = Prng::new(0);
        let first = z.next_u64();
        assert_ne!(first, 0, "the absorbing all-zero stream is unreachable");
        assert_eq!(first, seeded_zero.next_u64(), "remapped exactly as new(0)");
        assert_eq!(z.next_u64(), seeded_zero.next_u64());
    }
}
