// SPDX-License-Identifier: AGPL-3.0-or-later

//! Mutation shape: how many actions one job appends and where each comes from.
//!
//! The shape is search policy, so it lives here rather than in any one
//! target. A target supplies only the two draws the shape composes: one
//! action from its own alphabet, and one action offered by whatever biased
//! table the run maintains. Both are recorded identifiers, and a replay
//! re-derives every suffix from the recorded mutation seed alone.

use std::{error::Error, num::NonZeroUsize};

use crate::search::rand::RomuDuoJrRand;

/// Identifier recorded for the one-or-two suffix shape.
pub const SUFFIX_ONE_OR_TWO_IDENTIFIER: &str = "one_or_two";

/// Identifier recorded for the one-to-six suffix shape.
pub const SUFFIX_ONE_TO_SIX_IDENTIFIER: &str = "one_to_six";

/// How many actions one job appends to its parent's input.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SuffixShape {
    /// One action, or two at one-in-four odds.
    OneOrTwo,
    /// A uniform draw of one to six actions.
    #[default]
    OneToSix,
}

/// The recorded identifier of a suffix shape.
#[must_use]
pub fn suffix_shape_identifier(shape: SuffixShape) -> &'static str {
    match shape {
        SuffixShape::OneOrTwo => SUFFIX_ONE_OR_TWO_IDENTIFIER,
        SuffixShape::OneToSix => SUFFIX_ONE_TO_SIX_IDENTIFIER,
    }
}

/// The suffix shape a recorded identifier names.
///
/// # Errors
///
/// Returns an error when the identifier names no compiled shape.
pub fn suffix_shape_from_identifier(identifier: &str) -> Result<SuffixShape, Box<dyn Error>> {
    match identifier {
        SUFFIX_ONE_OR_TWO_IDENTIFIER => Ok(SuffixShape::OneOrTwo),
        SUFFIX_ONE_TO_SIX_IDENTIFIER => Ok(SuffixShape::OneToSix),
        _ => Err(format!("suffix shape {identifier} is not recognized").into()),
    }
}

/// Where each action of a suffix is drawn from.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DrawMixture {
    /// Every action from the target's own alphabet.
    #[default]
    AlphabetOnly,
    /// Half the actions offered by the run's biased table first, falling
    /// back to the alphabet whenever the table offers nothing.
    BiasedHalf,
    /// One strategy per suffix, drawn at the weight the campaign maintains:
    /// a strategy's share halves every `scale` consecutive suffixes that
    /// opened no new retention slot and resets when one does, flooring so
    /// both strategies always keep a live share. The chosen strategy
    /// supplies every action of the suffix, with the biased table still
    /// falling back to the alphabet when it offers nothing.
    Energy {
        /// Barren suffixes per halving of a strategy's share.
        scale: u64,
    },
    /// The energy mixture with a third strategy: splice the stored tail of a
    /// cell-mate's deepest descendant onto the selected parent. The splice
    /// strategy draws no actions of its own; when the archive offers no tail
    /// its suffix falls back to the alphabet.
    EnergySplice {
        /// Barren suffixes per halving of a strategy's share.
        scale: u64,
    },
}

/// Which strategy an energy draw assigned to one suffix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnergyStrategy {
    /// Every action offered by the biased table.
    Table,
    /// The suffix is a stored descendant tail from the parent's cell.
    Splice,
    /// Every action from the target's alphabet.
    Alphabet,
}

/// Identifier recorded for the alphabet-only mixture.
pub const MIXTURE_ALPHABET_ONLY_IDENTIFIER: &str = "alphabet_only";

/// Identifier recorded for the biased-half mixture.
pub const MIXTURE_BIASED_HALF_IDENTIFIER: &str = "biased_half";

/// Identifier prefix recorded for the energy mixture; the scale follows.
pub const MIXTURE_ENERGY_PREFIX: &str = "energy:";

/// Identifier prefix recorded for the energy mixture with splice.
pub const MIXTURE_ENERGY_SPLICE_PREFIX: &str = "energy_splice:";

/// The recorded identifier of a draw mixture.
#[must_use]
pub fn draw_mixture_identifier(mixture: DrawMixture) -> String {
    match mixture {
        DrawMixture::AlphabetOnly => MIXTURE_ALPHABET_ONLY_IDENTIFIER.to_owned(),
        DrawMixture::BiasedHalf => MIXTURE_BIASED_HALF_IDENTIFIER.to_owned(),
        DrawMixture::Energy { scale } => format!("{MIXTURE_ENERGY_PREFIX}{scale}"),
        DrawMixture::EnergySplice { scale } => format!("{MIXTURE_ENERGY_SPLICE_PREFIX}{scale}"),
    }
}

/// The draw mixture a recorded identifier names.
///
/// # Errors
///
/// Returns an error when the identifier names no compiled mixture.
pub fn draw_mixture_from_identifier(identifier: &str) -> Result<DrawMixture, Box<dyn Error>> {
    if let Some(scale) = identifier.strip_prefix(MIXTURE_ENERGY_SPLICE_PREFIX) {
        let scale = scale.parse::<u64>()?;
        if scale == 0 {
            return Err("energy mixture scale must be nonzero".into());
        }
        return Ok(DrawMixture::EnergySplice { scale });
    }
    if let Some(scale) = identifier.strip_prefix(MIXTURE_ENERGY_PREFIX) {
        let scale = scale.parse::<u64>()?;
        if scale == 0 {
            return Err("energy mixture scale must be nonzero".into());
        }
        return Ok(DrawMixture::Energy { scale });
    }
    match identifier {
        MIXTURE_ALPHABET_ONLY_IDENTIFIER => Ok(DrawMixture::AlphabetOnly),
        MIXTURE_BIASED_HALF_IDENTIFIER => Ok(DrawMixture::BiasedHalf),
        _ => Err(format!("draw mixture {identifier} is not recognized").into()),
    }
}

/// One suffix draw's mixture and the biased-strategy weight it was drawn
/// at; the weight only steers the energy mixture.
#[derive(Clone, Copy, Debug)]
pub struct MixtureDraw {
    /// The run's recorded mixture.
    pub mixture: DrawMixture,
    /// Biased-strategy weight out of 256 at this draw.
    pub weight: u8,
    /// Splice-strategy weight out of 256 at this draw; zero outside the
    /// splice mixture, which keeps older streams byte-identical.
    pub splice_weight: u8,
}

/// Per-stream shares of the energy mixture's two strategies. Live updates
/// the counters as job outcomes complete and records each draw's resulting
/// weight in the stream, so replay re-derives every suffix from the record
/// alone and never needs the counters.
#[derive(Clone, Copy, Debug, Default)]
pub struct MixtureEnergy {
    /// Consecutive suffixes per strategy (biased, splice, alphabet) that
    /// opened no new retention slot.
    barren: [u64; 3],
}

fn energy_share(barren: u64, scale: u64) -> u64 {
    let halvings = u32::try_from((barren / scale).min(8)).unwrap_or(8);
    (256_u64 >> halvings).max(1)
}

impl MixtureEnergy {
    /// The biased strategy's current weight out of 256 in the two-strategy
    /// energy mixture, kept off both ends so each strategy always has a
    /// live share.
    #[must_use]
    pub fn biased_weight(&self, scale: u64) -> u8 {
        let biased = energy_share(self.barren[0], scale);
        let total = biased + energy_share(self.barren[2], scale);
        u8::try_from(((256 * biased) / total).clamp(1, 255)).unwrap_or(128)
    }

    /// The (biased, splice) weights out of 256 in the three-strategy splice
    /// mixture. Each strategy keeps a live share and the alphabet keeps at
    /// least one point of the 256.
    #[must_use]
    pub fn splice_weights(&self, scale: u64) -> (u8, u8) {
        let shares = self.barren.map(|barren| energy_share(barren, scale));
        let total: u64 = shares.iter().sum();
        let weight = |share: u64| ((256 * share) / total).clamp(1, 253);
        let biased = weight(shares[0]);
        let splice = weight(shares[1]).min(254 - biased);
        (
            u8::try_from(biased).unwrap_or(85),
            u8::try_from(splice.max(1)).unwrap_or(85),
        )
    }

    /// Fold one suffix outcome into the strategy that drew it.
    pub fn record_outcome(&mut self, strategy: EnergyStrategy, new_slot: bool) {
        let index = match strategy {
            EnergyStrategy::Table => 0,
            EnergyStrategy::Splice => 1,
            EnergyStrategy::Alphabet => 2,
        };
        if new_slot {
            self.barren[index] = 0;
        } else {
            self.barren[index] = self.barren[index].saturating_add(1);
        }
    }
}

/// The strategy the energy mixtures assign to this suffix. The strategy
/// draw is the seeded generator's first draw, so the campaign re-derives it
/// at outcome time from the recorded seed and weights alone.
///
/// # Errors
///
/// Returns an error when the weight bound is invalid.
pub fn energy_strategy(
    mutation_seed: u64,
    biased_weight: u8,
    splice_weight: u8,
) -> Result<EnergyStrategy, Box<dyn Error>> {
    let mut rand = RomuDuoJrRand::with_seed(mutation_seed);
    let draw = rand.below(NonZeroUsize::new(256).ok_or("invalid mixture weight bound")?);
    if draw < usize::from(biased_weight) {
        return Ok(EnergyStrategy::Table);
    }
    if draw < usize::from(biased_weight) + usize::from(splice_weight) {
        return Ok(EnergyStrategy::Splice);
    }
    Ok(EnergyStrategy::Alphabet)
}

/// Whether the energy mixture assigns this suffix to the biased strategy.
///
/// # Errors
///
/// Returns an error when the weight bound is invalid.
pub fn energy_strategy_is_biased(
    mutation_seed: u64,
    biased_weight: u8,
) -> Result<bool, Box<dyn Error>> {
    Ok(energy_strategy(mutation_seed, biased_weight, 0)? == EnergyStrategy::Table)
}

/// Expand one mutation seed into a complete suffix.
///
/// The suffix is sampled from a fresh generator seeded with `mutation_seed`
/// alone, so a job is a pure function of (parent snapshot, mutation seed).
/// `biased` may decline without consuming a draw; it is only consulted on the
/// draws the mixture assigns to it.
///
/// # Errors
///
/// Returns an error when a draw bound is invalid or either draw fails.
pub fn draw_suffix<A, B, U>(
    shape: SuffixShape,
    mixture: DrawMixture,
    mixture_weight: u8,
    mutation_seed: u64,
    mut biased: B,
    mut alphabet: U,
) -> Result<Vec<A>, Box<dyn Error>>
where
    B: FnMut(&mut RomuDuoJrRand) -> Result<Option<A>, Box<dyn Error>>,
    U: FnMut(&mut RomuDuoJrRand) -> Result<A, Box<dyn Error>>,
{
    let mut rand = RomuDuoJrRand::with_seed(mutation_seed);
    // The energy strategy draw comes first so it is re-derivable from the
    // seed and recorded weights alone; see `energy_strategy`. A suffix the
    // splice strategy could not fill reaches this function and lands in the
    // alphabet arm, since the draw is below the table weight only for the
    // table strategy.
    let energy_biased = match mixture {
        DrawMixture::Energy { .. } | DrawMixture::EnergySplice { .. } => Some(
            rand.below(NonZeroUsize::new(256).ok_or("invalid mixture weight bound")?)
                < usize::from(mixture_weight),
        ),
        DrawMixture::AlphabetOnly | DrawMixture::BiasedHalf => None,
    };
    let length = match shape {
        SuffixShape::OneOrTwo => {
            if rand.below(NonZeroUsize::new(4).ok_or("invalid suffix odds")?) == 0 {
                2
            } else {
                1
            }
        }
        SuffixShape::OneToSix => 1 + rand.below(NonZeroUsize::new(6).ok_or("invalid suffix odds")?),
    };
    let mut suffix = Vec::with_capacity(length);
    for _ in 0..length {
        let take_biased = match energy_biased {
            Some(biased_strategy) => biased_strategy,
            None => {
                mixture == DrawMixture::BiasedHalf
                    && rand.below(NonZeroUsize::new(2).ok_or("invalid mixture odds")?) == 0
            }
        };
        if take_biased && let Some(action) = biased(&mut rand)? {
            suffix.push(action);
            continue;
        }
        suffix.push(alphabet(&mut rand)?);
    }
    Ok(suffix)
}

#[cfg(test)]
mod tests {
    use super::{
        DrawMixture, EnergyStrategy, MixtureEnergy, SuffixShape, draw_mixture_from_identifier,
        draw_mixture_identifier, draw_suffix, energy_strategy, energy_strategy_is_biased,
        suffix_shape_from_identifier, suffix_shape_identifier,
    };

    #[test]
    fn the_shape_identifier_round_trips_and_rejects_unknown_names() {
        let shape = SuffixShape::OneOrTwo;
        assert_eq!(
            suffix_shape_from_identifier(suffix_shape_identifier(shape)).expect("round trip"),
            shape
        );
        assert!(suffix_shape_from_identifier("two_or_three").is_err());
    }

    #[test]
    fn the_mixture_identifier_round_trips_and_rejects_unknown_names() {
        for mixture in [
            DrawMixture::AlphabetOnly,
            DrawMixture::BiasedHalf,
            DrawMixture::Energy { scale: 6 },
            DrawMixture::EnergySplice { scale: 6 },
        ] {
            assert_eq!(
                draw_mixture_from_identifier(&draw_mixture_identifier(mixture))
                    .expect("round trip"),
                mixture
            );
        }
        assert!(draw_mixture_from_identifier("table_only").is_err());
        assert!(draw_mixture_from_identifier("energy:0").is_err());
    }

    /// A biased table that offers nothing must consume no draw, so a run
    /// whose table is still empty draws exactly what the alphabet-only
    /// mixture would.
    #[test]
    fn a_declining_biased_draw_consumes_nothing() {
        for seed in 0..512_u64 {
            let mut alphabet_calls = 0_u32;
            let plain = draw_suffix(
                SuffixShape::OneOrTwo,
                DrawMixture::AlphabetOnly,
                128,
                seed,
                |_| Ok(None::<u64>),
                |rand| {
                    alphabet_calls += 1;
                    Ok(rand.next_u64())
                },
            )
            .expect("alphabet-only suffix");
            let with_empty_table = draw_suffix(
                SuffixShape::OneOrTwo,
                DrawMixture::BiasedHalf,
                128,
                seed,
                |_| Ok(None::<u64>),
                |rand| Ok(rand.next_u64()),
            )
            .expect("biased-half suffix over an empty table");
            assert_eq!(plain.len(), alphabet_calls as usize);
            assert_ne!(plain, with_empty_table);
        }
    }

    /// One in four seeds asks for two actions.
    #[test]
    fn two_action_suffixes_are_drawn_at_one_in_four() {
        let long = (0..4_096_u64)
            .filter(|seed| {
                draw_suffix(
                    SuffixShape::OneOrTwo,
                    DrawMixture::AlphabetOnly,
                    128,
                    *seed,
                    |_| Ok(None::<u64>),
                    |rand| Ok(rand.next_u64()),
                )
                .expect("suffix")
                .len()
                    == 2
            })
            .count();
        assert!((900..1_150).contains(&long), "two-action suffixes: {long}");
    }

    /// The energy mixture assigns each suffix to one strategy, matches the
    /// re-derivation helper, and follows the recorded weight.
    #[test]
    fn the_energy_mixture_follows_its_recorded_weight() {
        for (weight, low, high) in [(255_u8, 4_000, 4_096), (1, 0, 96), (128, 1_850, 2_250)] {
            let biased = (0..4_096_u64)
                .filter(|seed| {
                    let suffix = draw_suffix(
                        SuffixShape::OneToSix,
                        DrawMixture::Energy { scale: 6 },
                        weight,
                        *seed,
                        |_| Ok(Some(1_u64)),
                        |_| Ok(0_u64),
                    )
                    .expect("energy suffix");
                    let from_table = suffix.iter().all(|action| *action == 1);
                    assert!(
                        from_table || suffix.iter().all(|action| *action == 0),
                        "a suffix must draw every action from one strategy"
                    );
                    assert_eq!(
                        from_table,
                        energy_strategy_is_biased(*seed, weight).expect("strategy"),
                        "the helper must re-derive the strategy draw"
                    );
                    from_table
                })
                .count();
            assert!(
                (low..=high).contains(&biased),
                "weight {weight} chose the table {biased} times"
            );
        }
    }

    /// Barren streaks move the shared weight and a new slot resets them.
    #[test]
    fn mixture_energy_counters_move_the_weight() {
        let mut energy = MixtureEnergy::default();
        assert_eq!(energy.biased_weight(6), 128);
        for _ in 0..12 {
            energy.record_outcome(EnergyStrategy::Table, false);
        }
        assert!(energy.biased_weight(6) < 70);
        energy.record_outcome(EnergyStrategy::Table, true);
        assert_eq!(energy.biased_weight(6), 128);
        for _ in 0..60 {
            energy.record_outcome(EnergyStrategy::Alphabet, false);
        }
        assert!(energy.biased_weight(6) > 240);
    }

    /// The three-strategy weights shift toward whichever strategy keeps
    /// discovering, the strategy helper follows the recorded segments, and
    /// every strategy keeps a live share.
    #[test]
    fn splice_weights_shift_between_three_strategies() {
        let mut energy = MixtureEnergy::default();
        let (table, splice) = energy.splice_weights(6);
        assert_eq!((table, splice), (85, 85));
        for _ in 0..60 {
            energy.record_outcome(EnergyStrategy::Splice, false);
        }
        let (table, splice) = energy.splice_weights(6);
        assert!(splice <= 2, "a cold splice share must collapse: {splice}");
        assert!(table > 100);
        energy.record_outcome(EnergyStrategy::Splice, true);
        let (_, splice) = energy.splice_weights(6);
        assert_eq!(splice, 85);
        let mut counts = [0_u32; 3];
        for seed in 0..4_096_u64 {
            match energy_strategy(seed, 85, 85).expect("strategy") {
                EnergyStrategy::Table => counts[0] += 1,
                EnergyStrategy::Splice => counts[1] += 1,
                EnergyStrategy::Alphabet => counts[2] += 1,
            }
        }
        for count in counts {
            assert!(
                (1_100..=1_650).contains(&count),
                "strategy counts {counts:?}"
            );
        }
    }
}
