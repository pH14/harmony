// SPDX-License-Identifier: AGPL-3.0-or-later
//! The local xorshift64\* generator that drives the per-connection [`Loss`] roll.
//!
//! This is exactly the `hypercall-proto` deterministic-entropy algorithm,
//! re-implemented locally (conventions rule 2 — no sibling dependency) so the
//! drop sampling is portable and golden-testable. xorshift64\* is a bijection on
//! the nonzero state space, so a normalized seed never collapses the stream to
//! zero. The same seed always yields the same stream, which is the whole point:
//! a [`Loss`] policy seeded from the (recorded) decision drops the *same* chunks
//! on every replay.
//!
//! [`Loss`]: crate::FlowPolicy::Loss

/// xorshift64\* multiplier (the `hypercall-proto` constant).
const MUL: u64 = 0x2545_F491_4F6C_DD1D;
/// Seed substituted for a zero seed, so the nonzero-state invariant holds. The
/// golden-ratio constant, matching `hypercall-proto`'s fallback.
const FALLBACK: u64 = 0x9E37_79B9_7F4A_7C15;

/// A deterministic xorshift64\* stream. Each [`next_u64`](Prng::next_u64) both
/// advances the state and returns the scrambled output word.
#[derive(Clone, Debug)]
pub(crate) struct Prng {
    state: u64,
}

impl Prng {
    /// Start a stream from `seed` (zero is remapped to a fixed nonzero seed so
    /// the xorshift state can never collapse).
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { FALLBACK } else { seed },
        }
    }

    /// Advance the stream and return the next 64-bit output.
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(MUL)
    }
}
