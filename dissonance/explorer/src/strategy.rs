// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pluggable exploration policy — seed-only, coverage-guided, or (by the same
//! seam) human/replay all fit.
//!
//! A [`Strategy`] answers two questions: inside a Timeline, how to answer one
//! surfaced decision ([`choose`](Strategy::choose)); and across the Multiverse,
//! which [`Environment`] to try next ([`next_env`](Strategy::next_env)). Both are
//! driven only by a caller-seeded PRNG and the (deterministic) coverage/corpus
//! state, so a campaign is a pure function of `(strategy seed, machine)` — there
//! is no `rand`, no wall-clock, no host entropy (conventions rule 4). Mutation
//! lives **here**, never in the wire, and always goes through the [`EnvCodec`] so
//! every minted blob is valid (the AFL lesson).

use crate::prng::Prng;
use crate::seam::EnvCodec;
use crate::{Answer, Corpus, Environment, SnapId};

/// The exploration policy seam. A campaign drives whichever implementation it
/// chooses; the engine is generic over it.
pub trait Strategy {
    /// Answer one surfaced decision (the Timeline's inner step). `ctx` is opaque
    /// service↔policy bytes; `coverage` is the live AFL-style map, so a
    /// coverage-guided policy can steer toward novelty. Deterministic given the
    /// strategy's own PRNG state.
    fn choose(&mut self, ctx: &[u8], coverage: &[u8]) -> Answer;

    /// Produce the next [`Environment`] to try (the Multiverse's outer step):
    /// mutate a corpus entry or draw a fresh seed, **minting it through `env`** so
    /// the blob is always valid (the strategy decides the seed / mutation, the
    /// codec encodes task 24's structure). On an **empty corpus** (step 1) it
    /// returns `(genesis, env.seeded(..))` — `genesis` is the only valid base
    /// before anything is admitted, so it is passed in explicitly.
    fn next_env(
        &mut self,
        corpus: &Corpus,
        genesis: SnapId,
        env: &dyn EnvCodec,
    ) -> (SnapId, Environment);
}

/// Pure seed-driven exploration (FoundationDB style): every Multiverse step draws
/// a fresh seed and branches from genesis; it never overrides a decision, so —
/// run with [`StopMask::NONE`](crate::StopMask::NONE) — a Timeline has zero stops
/// and the recorded [`Environment`] is a pure seed. The reproducible artifact is
/// the seed alone.
#[derive(Clone, Debug)]
pub struct SeedStrategy {
    seeds: Prng,
}

impl SeedStrategy {
    /// A campaign seeded by `seed`; each step's fresh environment seed is drawn
    /// from it deterministically.
    pub fn new(seed: u64) -> Self {
        Self {
            seeds: Prng::new(seed),
        }
    }
}

impl Strategy for SeedStrategy {
    /// Declines: a pure seed-driven strategy never overrides a decision, so it
    /// returns an empty [`Answer`] (the backing falls through to its seed). If a
    /// campaign surfaces a decision to a `SeedStrategy` anyway, it is answered by
    /// the seed, not pinned — keeping the artifact a pure seed.
    fn choose(&mut self, _ctx: &[u8], _coverage: &[u8]) -> Answer {
        Answer(Vec::new())
    }

    /// Always a fresh genesis seed — the corpus is ignored (pure DST).
    fn next_env(
        &mut self,
        _corpus: &Corpus,
        genesis: SnapId,
        env: &dyn EnvCodec,
    ) -> (SnapId, Environment) {
        (genesis, env.seeded(self.seeds.next_u64()))
    }
}

/// Every Nth Multiverse step draws a fresh genesis seed (exploration); the rest
/// mutate a corpus entry (exploitation), so a `CoverageStrategy` campaign always
/// has a genesis base to admit new snapshots from. Tunable via
/// [`with_explore_period`](CoverageStrategy::with_explore_period).
const DEFAULT_EXPLORE_PERIOD: u64 = 3;

/// Coverage-guided exploration (Antithesis style): novelty-steered
/// [`choose`](Strategy::choose) and a mutate/seed mix in
/// [`next_env`](Strategy::next_env). It branches from corpus snapshots to exploit
/// known-interesting prefixes and periodically draws a fresh genesis seed to keep
/// discovering new ones.
#[derive(Clone, Debug)]
pub struct CoverageStrategy {
    rng: Prng,
    step: u64,
    explore_period: u64,
}

impl CoverageStrategy {
    /// A campaign seeded by `seed`.
    pub fn new(seed: u64) -> Self {
        Self {
            rng: Prng::new(seed),
            step: 0,
            explore_period: DEFAULT_EXPLORE_PERIOD,
        }
    }

    /// Set how often (every Nth step) the strategy draws a fresh genesis seed
    /// instead of mutating a corpus entry. Clamped to at least one. Additional
    /// helper for tuning exploration vs exploitation.
    pub fn with_explore_period(mut self, period: u64) -> Self {
        self.explore_period = period.max(1);
        self
    }
}

impl Strategy for CoverageStrategy {
    /// Draw a decision answer from the PRNG, folding a checksum of the live
    /// coverage and `ctx` into the draw so the choice is coverage-guided yet
    /// deterministic.
    fn choose(&mut self, ctx: &[u8], coverage: &[u8]) -> Answer {
        let mix = checksum(coverage) ^ checksum(ctx);
        let r = self.rng.next_u64() ^ mix;
        Answer(vec![(r & 0xff) as u8])
    }

    /// Exploit a corpus snapshot most steps; explore a fresh genesis seed every
    /// `explore_period` steps (and whenever the corpus is empty).
    fn next_env(
        &mut self,
        corpus: &Corpus,
        genesis: SnapId,
        env: &dyn EnvCodec,
    ) -> (SnapId, Environment) {
        self.step = self.step.wrapping_add(1);
        let explore = corpus.is_empty() || self.step.is_multiple_of(self.explore_period);
        if explore {
            return (genesis, env.seeded(self.rng.next_u64()));
        }
        // Exploit: pick a corpus entry by one draw, mutate it by another.
        let pick = self.rng.next_u64();
        let salt = self.rng.next_u64();
        match corpus.select(pick) {
            Some((snap, base)) => (snap, env.mutate(base, salt)),
            // Unreachable (corpus is non-empty here), but stay total rather than
            // unwrap: fall back to a fresh genesis seed.
            None => (genesis, env.seeded(salt)),
        }
    }
}

/// A tiny order-independent checksum used only to fold coverage/ctx into a
/// strategy draw (FNV-1a). It never reaches an encoded byte or a kept set, so it
/// is not a determinism surface beyond being a pure function of its input.
fn checksum(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal in-module [`EnvCodec`] that records the seed/salt into the blob,
    /// so a test can tell `seeded` (explore) from `mutate` (exploit).
    struct TestCodec;
    impl EnvCodec for TestCodec {
        fn seeded(&self, seed: u64) -> Environment {
            Environment {
                blob_version: 1,
                bytes: seed.to_le_bytes().to_vec(),
            }
        }
        fn mutate(&self, base: &Environment, salt: u64) -> Environment {
            let mut bytes = base.bytes.clone();
            bytes.extend_from_slice(&salt.to_le_bytes());
            Environment {
                blob_version: 2,
                bytes,
            }
        }
        fn compose(&self, base: &Environment, _branch_local: &Environment) -> Environment {
            base.clone()
        }
    }

    /// FNV-1a, pinned exactly — empty input is the offset basis, and a known input
    /// is its golden digest (locks the `^=`/`*=` fold and the != 0/1 return).
    #[test]
    fn checksum_is_pinned() {
        assert_eq!(checksum(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(checksum(b"abc"), 0xe71f_a219_0541_574b);
    }

    /// `CoverageStrategy::choose` is an exact byte for known `(seed, ctx, cov)` —
    /// pins the `next_u64() ^ checksum(cov) ^ checksum(ctx)` fold (a `|`/`&` swap
    /// changes the byte).
    #[test]
    fn coverage_choose_is_a_pinned_byte() {
        let mut s = CoverageStrategy::new(0x1234);
        assert_eq!(s.choose(&[7, 9], &[1, 2, 3]), Answer(vec![206]));
    }

    /// `SeedStrategy::choose` always declines with an empty answer.
    #[test]
    fn seed_choose_is_always_empty() {
        let mut s = SeedStrategy::new(7);
        assert_eq!(s.choose(&[1, 2, 3], &[4, 5, 6]), Answer(vec![]));
        assert_eq!(s.choose(&[], &[]), Answer(vec![]));
    }

    /// `next_env` exploits a non-empty corpus off-period and explores (returns the
    /// genesis snap) on the period boundary — pins the `is_empty() || multiple`
    /// decision (an `&&` swap would exploit on the boundary too).
    #[test]
    fn next_env_explore_vs_exploit_is_pinned() {
        let codec = TestCodec;
        let mut corpus = Corpus::new();
        // One entry at a distinctly non-genesis snap.
        assert!(corpus.admit(SnapId(42), codec.seeded(0), &[1, 1, 1]));
        let genesis = SnapId(999);

        let mut s = CoverageStrategy::new(5).with_explore_period(2);
        // Step 1: 1 % 2 != 0 → exploit → the corpus entry's snap.
        let (snap1, _) = s.next_env(&corpus, genesis, &codec);
        assert_eq!(snap1, SnapId(42), "an off-period step exploits the corpus");
        // Step 2: 2 % 2 == 0 → explore → genesis.
        let (snap2, _) = s.next_env(&corpus, genesis, &codec);
        assert_eq!(snap2, genesis, "the period boundary explores from genesis");

        // An empty corpus always explores, whatever the step.
        let empty = Corpus::new();
        let mut s2 = CoverageStrategy::new(5).with_explore_period(100);
        let (snap, _) = s2.next_env(&empty, genesis, &codec);
        assert_eq!(snap, genesis, "an empty corpus explores from genesis");
    }
}
