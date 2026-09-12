// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, num::NonZeroUsize};

use crate::search::rand::RomuDuoJrRand;

pub const SUFFIX_ONE_OR_TWO_IDENTIFIER: &str = "one_or_two";

pub const SUFFIX_ONE_TO_SIX_IDENTIFIER: &str = "one_to_six";

pub const SUFFIX_ONE_TO_SIX_BOUNDED_IDENTIFIER: &str =
    "one_to_six_within_3_longest_actions_full_hold";

pub const SUFFIX_TIME_BOUND_LONGEST_ACTIONS: u64 = 3;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SuffixShape {
    OneOrTwo,
    OneToSix,
    #[default]
    OneToSixBounded,
}

impl SuffixShape {
    pub(crate) fn bound_time<A>(
        self,
        suffix: &mut Vec<A>,
        time: fn(&A) -> u64,
        longest_action_time: u64,
    ) {
        if self != Self::OneToSixBounded {
            return;
        }
        let bound = SUFFIX_TIME_BOUND_LONGEST_ACTIONS.saturating_mul(longest_action_time);
        let mut total = 0_u64;
        let reached = suffix.iter().position(|action| {
            total = total.saturating_add(time(action));
            total >= bound
        });
        if let Some(index) = reached {
            suffix.truncate(index.saturating_add(1));
        }
    }
}

#[must_use]
pub(crate) fn suffix_shape_identifier(shape: SuffixShape) -> &'static str {
    match shape {
        SuffixShape::OneOrTwo => SUFFIX_ONE_OR_TWO_IDENTIFIER,
        SuffixShape::OneToSix => SUFFIX_ONE_TO_SIX_IDENTIFIER,
        SuffixShape::OneToSixBounded => SUFFIX_ONE_TO_SIX_BOUNDED_IDENTIFIER,
    }
}

pub fn suffix_shape_from_identifier(identifier: &str) -> Result<SuffixShape, Box<dyn Error>> {
    match identifier {
        SUFFIX_ONE_OR_TWO_IDENTIFIER => Ok(SuffixShape::OneOrTwo),
        SUFFIX_ONE_TO_SIX_IDENTIFIER => Ok(SuffixShape::OneToSix),
        SUFFIX_ONE_TO_SIX_BOUNDED_IDENTIFIER => Ok(SuffixShape::OneToSixBounded),
        _ => Err(format!("suffix shape {identifier} is not recognized").into()),
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DrawMixture {
    EnergySpliceContinuation {
        scale: u64,
    },
    #[default]
    AlphabetOnly,
    BiasedHalf,
    Energy {
        scale: u64,
    },
    EnergySplice {
        scale: u64,
    },
    AlphabetContinuation,
    EnergySpliceContinuationIsolated {
        scale: u64,
    },
}

impl DrawMixture {
    pub(crate) fn isolates_continuations(self) -> bool {
        matches!(
            self,
            Self::AlphabetContinuation | Self::EnergySpliceContinuationIsolated { .. }
        )
    }

    pub(crate) fn uses_continuations(self) -> bool {
        matches!(
            self,
            Self::EnergySpliceContinuation { .. }
                | Self::AlphabetContinuation
                | Self::EnergySpliceContinuationIsolated { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EnergyStrategy {
    Table,
    Splice,
    Alphabet,
}

pub const MIXTURE_ALPHABET_ONLY_IDENTIFIER: &str = "alphabet_only";

pub const MIXTURE_BIASED_HALF_IDENTIFIER: &str = "biased_half";

pub const MIXTURE_ENERGY_PREFIX: &str = "energy:";

pub const MIXTURE_ENERGY_SPLICE_PREFIX: &str = "energy_splice:";

#[must_use]
pub(crate) fn draw_mixture_identifier(mixture: DrawMixture) -> String {
    match mixture {
        DrawMixture::EnergySpliceContinuation { scale } => {
            format!("energy_splice_continuation_v1:{scale}")
        }
        DrawMixture::EnergySpliceContinuationIsolated { scale } => {
            format!("energy_splice_continuation_v2:{scale}")
        }
        DrawMixture::AlphabetOnly => MIXTURE_ALPHABET_ONLY_IDENTIFIER.to_owned(),
        DrawMixture::AlphabetContinuation => "alphabet_continuation_v1".to_owned(),
        DrawMixture::BiasedHalf => MIXTURE_BIASED_HALF_IDENTIFIER.to_owned(),
        DrawMixture::Energy { scale } => format!("{MIXTURE_ENERGY_PREFIX}{scale}"),
        DrawMixture::EnergySplice { scale } => format!("{MIXTURE_ENERGY_SPLICE_PREFIX}{scale}"),
    }
}

pub fn draw_mixture_from_identifier(identifier: &str) -> Result<DrawMixture, Box<dyn Error>> {
    if let Some(scale) = identifier.strip_prefix("energy_splice_continuation_v2:") {
        let scale = scale.parse::<u64>()?;
        if scale == 0 {
            return Err("energy mixture scale must be nonzero".into());
        }
        return Ok(DrawMixture::EnergySpliceContinuationIsolated { scale });
    }
    if let Some(scale) = identifier.strip_prefix("energy_splice_continuation_v1:") {
        let scale = scale.parse::<u64>()?;
        if scale == 0 {
            return Err("energy mixture scale must be nonzero".into());
        }
        return Ok(DrawMixture::EnergySpliceContinuation { scale });
    }
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
        "alphabet_continuation_v1" => Ok(DrawMixture::AlphabetContinuation),
        MIXTURE_BIASED_HALF_IDENTIFIER => Ok(DrawMixture::BiasedHalf),
        _ => Err(format!("draw mixture {identifier} is not recognized").into()),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MixtureDraw {
    pub mixture: DrawMixture,
    pub weight: u8,
    pub splice_weight: u8,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MixtureEnergy {
    barren: [u64; 3],
}

fn energy_share(barren: u64, scale: u64) -> u64 {
    let halvings = u32::try_from((barren / scale).min(8)).unwrap_or(8);
    (256_u64 >> halvings).max(1)
}

impl MixtureEnergy {
    #[must_use]
    pub(crate) fn biased_weight(&self, scale: u64) -> u8 {
        let biased = energy_share(self.barren[0], scale);
        let total = biased + energy_share(self.barren[2], scale);
        u8::try_from(((256 * biased) / total).clamp(1, 255)).unwrap_or(128)
    }

    #[must_use]
    pub(crate) fn splice_weights(&self, scale: u64) -> (u8, u8) {
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

    pub(crate) fn record_outcome(&mut self, strategy: EnergyStrategy, new_slot: bool) {
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

pub(crate) fn energy_strategy(
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
    let energy_biased = match mixture {
        DrawMixture::Energy { .. }
        | DrawMixture::EnergySplice { .. }
        | DrawMixture::EnergySpliceContinuation { .. }
        | DrawMixture::EnergySpliceContinuationIsolated { .. } => Some(
            rand.below(NonZeroUsize::new(256).ok_or("invalid mixture weight bound")?)
                < usize::from(mixture_weight),
        ),
        DrawMixture::AlphabetOnly | DrawMixture::AlphabetContinuation | DrawMixture::BiasedHalf => {
            None
        }
    };
    let length = match shape {
        SuffixShape::OneOrTwo => {
            if rand.below(NonZeroUsize::new(4).ok_or("invalid suffix odds")?) == 0 {
                2
            } else {
                1
            }
        }
        SuffixShape::OneToSix | SuffixShape::OneToSixBounded => {
            1 + rand.below(NonZeroUsize::new(6).ok_or("invalid suffix odds")?)
        }
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
        draw_mixture_identifier, draw_suffix, energy_strategy, suffix_shape_from_identifier,
        suffix_shape_identifier,
    };

    #[test]
    fn the_bounded_shape_cuts_a_suffix_after_the_action_that_reaches_the_bound() {
        let time = |action: &u64| *action;
        let longest = 120;
        let mut suffix = vec![100, 200, 60, 5, 5];
        SuffixShape::OneToSixBounded.bound_time(&mut suffix, time, longest);
        assert_eq!(suffix, vec![100, 200, 60]);
        let mut short = vec![100, 200, 59];
        SuffixShape::OneToSixBounded.bound_time(&mut short, time, longest);
        assert_eq!(short, vec![100, 200, 59]);
        let mut one = vec![1_000, 1];
        SuffixShape::OneToSixBounded.bound_time(&mut one, time, longest);
        assert_eq!(one, vec![1_000]);
        let mut unbounded = vec![100, 200, 60, 5, 5];
        SuffixShape::OneToSix.bound_time(&mut unbounded, time, longest);
        assert_eq!(unbounded.len(), 5);
    }

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
            DrawMixture::EnergySpliceContinuation { scale: 6 },
            DrawMixture::AlphabetContinuation,
            DrawMixture::EnergySpliceContinuationIsolated { scale: 6 },
        ] {
            assert_eq!(
                draw_mixture_from_identifier(&draw_mixture_identifier(mixture))
                    .expect("round trip"),
                mixture
            );
        }
        assert!(draw_mixture_from_identifier("table_only").is_err());
        assert!(draw_mixture_from_identifier("energy:0").is_err());
        assert!(draw_mixture_from_identifier("energy_splice_continuation_v2:0").is_err());
    }

    #[test]
    fn isolated_energy_continuation_keeps_ordinary_suffixes_identical() {
        for seed in 0..512 {
            for shape in [SuffixShape::OneOrTwo, SuffixShape::OneToSix] {
                let draw = |mixture| {
                    draw_suffix(
                        shape,
                        mixture,
                        85,
                        seed,
                        |rand| Ok(Some(rand.next_u64())),
                        |rand| Ok(rand.next_u64()),
                    )
                    .unwrap()
                };
                assert_eq!(
                    draw(DrawMixture::EnergySpliceContinuation { scale: 6 }),
                    draw(DrawMixture::EnergySpliceContinuationIsolated { scale: 6 })
                );
            }
        }
    }

    #[test]
    fn alphabet_continuation_keeps_ordinary_suffixes_identical() {
        for seed in 0..512 {
            for shape in [SuffixShape::OneOrTwo, SuffixShape::OneToSix] {
                let draw = |mixture| {
                    draw_suffix(
                        shape,
                        mixture,
                        255,
                        seed,
                        |_| panic!("alphabet continuation consulted a biased table"),
                        |rand| Ok(rand.next_u64()),
                    )
                    .unwrap()
                };
                assert_eq!(
                    draw(DrawMixture::AlphabetOnly),
                    draw(DrawMixture::AlphabetContinuation)
                );
            }
        }
    }

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
                    from_table
                })
                .count();
            assert!(
                (low..=high).contains(&biased),
                "weight {weight} chose the table {biased} times"
            );
        }
    }

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
