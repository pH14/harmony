// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, num::NonZeroUsize};

use crate::search::rand::RomuDuoJrRand;

pub const SUFFIX_DOUBLING_LIMIT: u8 = 64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DrawMixture {
    #[default]
    AlphabetOnly,
    BiasedHalf,
    Energy {
        scale: u64,
    },
    EnergySplice {
        scale: u64,
    },
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
        DrawMixture::AlphabetOnly => MIXTURE_ALPHABET_ONLY_IDENTIFIER.to_owned(),
        DrawMixture::BiasedHalf => MIXTURE_BIASED_HALF_IDENTIFIER.to_owned(),
        DrawMixture::Energy { scale } => format!("{MIXTURE_ENERGY_PREFIX}{scale}"),
        DrawMixture::EnergySplice { scale } => format!("{MIXTURE_ENERGY_SPLICE_PREFIX}{scale}"),
    }
}

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

#[derive(Clone, Copy, Debug)]
pub struct MixtureDraw {
    pub mixture: DrawMixture,
    pub weight: u8,
    pub splice_weight: u8,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
pub(crate) struct MixtureEnergy {
    barren_work: [u64; 3],
    jobs: u64,
    work: u64,
}

impl MixtureEnergy {
    fn share(&self, index: usize, scale: u64) -> u64 {
        let spent = u128::from(self.barren_work[index]) * u128::from(self.jobs);
        let unit = u128::from(scale) * u128::from(self.work);
        let halvings = spent.checked_div(unit).unwrap_or(0).min(8);
        (256_u64 >> u32::try_from(halvings).unwrap_or(8)).max(1)
    }

    #[must_use]
    pub(crate) fn biased_weight(&self, scale: u64) -> u8 {
        let biased = self.share(0, scale);
        let total = biased + self.share(2, scale);
        u8::try_from(((256 * biased) / total).clamp(1, 255)).unwrap_or(128)
    }

    #[must_use]
    pub(crate) fn splice_weights(&self, scale: u64) -> (u8, u8) {
        let shares = [0, 1, 2].map(|index| self.share(index, scale));
        let total: u64 = shares.iter().sum();
        let weight = |share: u64| ((256 * share) / total).clamp(1, 253);
        let biased = weight(shares[0]);
        let splice = weight(shares[1]).min(254 - biased);
        (
            u8::try_from(biased).unwrap_or(85),
            u8::try_from(splice.max(1)).unwrap_or(85),
        )
    }

    pub(crate) fn record_outcome(&mut self, strategy: EnergyStrategy, new_slot: bool, work: u64) {
        let index = match strategy {
            EnergyStrategy::Table => 0,
            EnergyStrategy::Splice => 1,
            EnergyStrategy::Alphabet => 2,
        };
        self.jobs = self.jobs.saturating_add(1);
        self.work = self.work.saturating_add(work);
        if new_slot {
            self.barren_work[index] = 0;
        } else {
            self.barren_work[index] = self.barren_work[index].saturating_add(work);
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
    mixture: DrawMixture,
    mixture_weight: u8,
    mutation_seed: u64,
    previous: Option<&A>,
    mut biased: B,
    mut alphabet: U,
) -> Result<Vec<A>, Box<dyn Error>>
where
    B: FnMut(&mut RomuDuoJrRand) -> Result<Option<A>, Box<dyn Error>>,
    U: FnMut(Option<&A>, &mut RomuDuoJrRand) -> Result<A, Box<dyn Error>>,
{
    let mut rand = RomuDuoJrRand::with_seed(mutation_seed);
    let energy_biased = match mixture {
        DrawMixture::Energy { .. } | DrawMixture::EnergySplice { .. } => Some(
            rand.below(NonZeroUsize::new(256).ok_or("invalid mixture weight bound")?)
                < usize::from(mixture_weight),
        ),
        DrawMixture::AlphabetOnly | DrawMixture::BiasedHalf => None,
    };
    let length = usize::from(SUFFIX_DOUBLING_LIMIT);
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
        let action = alphabet(suffix.last().or(previous), &mut rand)?;
        suffix.push(action);
    }
    Ok(suffix)
}

#[cfg(test)]
mod tests {
    use super::{
        DrawMixture, EnergyStrategy, MixtureEnergy, draw_mixture_from_identifier,
        draw_mixture_identifier, draw_suffix, energy_strategy,
    };

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
        assert!(draw_mixture_from_identifier("energy_splice_continuation_v2:0").is_err());
    }

    #[test]
    fn a_declining_biased_draw_consumes_nothing() {
        for seed in 0..512_u64 {
            let mut alphabet_calls = 0_u32;
            let plain = draw_suffix(
                DrawMixture::AlphabetOnly,
                128,
                seed,
                None,
                |_| Ok(None::<u64>),
                |_, rand| {
                    alphabet_calls += 1;
                    Ok(rand.next_u64())
                },
            )
            .expect("alphabet-only suffix");
            let with_empty_table = draw_suffix(
                DrawMixture::BiasedHalf,
                128,
                seed,
                None,
                |_| Ok(None::<u64>),
                |_, rand| Ok(rand.next_u64()),
            )
            .expect("biased-half suffix over an empty table");
            assert_eq!(plain.len(), alphabet_calls as usize);
            assert_ne!(plain, with_empty_table);
        }
    }

    #[test]
    fn each_alphabet_draw_sees_the_action_before_it() {
        for seed in 0..256_u64 {
            let mut seen = Vec::new();
            let suffix = draw_suffix(
                DrawMixture::AlphabetOnly,
                128,
                seed,
                Some(&7_u64),
                |_| Ok(None::<u64>),
                |previous, rand| {
                    seen.push(previous.copied());
                    Ok(rand.next_u64())
                },
            )
            .expect("suffix");
            let mut expected = vec![Some(7)];
            expected.extend(suffix[..suffix.len() - 1].iter().copied().map(Some));
            assert_eq!(seen, expected);
        }
    }

    #[test]
    fn the_energy_mixture_follows_its_recorded_weight() {
        for (weight, low, high) in [(255_u8, 4_000, 4_096), (1, 0, 96), (128, 1_850, 2_250)] {
            let biased = (0..4_096_u64)
                .filter(|seed| {
                    let suffix = draw_suffix(
                        DrawMixture::Energy { scale: 6 },
                        weight,
                        *seed,
                        None,
                        |_| Ok(Some(1_u64)),
                        |_, _| Ok(0_u64),
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
            energy.record_outcome(EnergyStrategy::Table, false, 10);
        }
        assert!(energy.biased_weight(6) < 70);
        energy.record_outcome(EnergyStrategy::Table, true, 10);
        assert_eq!(energy.biased_weight(6), 128);
        for _ in 0..60 {
            energy.record_outcome(EnergyStrategy::Alphabet, false, 10);
        }
        assert!(energy.biased_weight(6) > 240);
    }

    #[test]
    fn a_strategy_that_spends_more_work_per_job_loses_share_sooner() {
        let mut energy = MixtureEnergy::default();
        for _ in 0..6 {
            energy.record_outcome(EnergyStrategy::Table, false, 10);
            energy.record_outcome(EnergyStrategy::Splice, false, 300);
            energy.record_outcome(EnergyStrategy::Alphabet, false, 10);
        }
        let (table, splice) = energy.splice_weights(6);
        assert!(
            splice * 3 < table,
            "six costly barren splices outweigh six cheap barren draws: {table} {splice}"
        );
        energy.record_outcome(EnergyStrategy::Splice, true, 300);
        let (table, splice) = energy.splice_weights(6);
        assert_eq!(
            splice, table,
            "a splice that opens a slot recovers its share"
        );
    }

    #[test]
    fn splice_weights_shift_between_three_strategies() {
        let mut energy = MixtureEnergy::default();
        let (table, splice) = energy.splice_weights(6);
        assert_eq!((table, splice), (85, 85));
        for _ in 0..60 {
            energy.record_outcome(EnergyStrategy::Splice, false, 10);
        }
        let (table, splice) = energy.splice_weights(6);
        assert!(splice <= 2, "a cold splice share must collapse: {splice}");
        assert!(table > 100);
        energy.record_outcome(EnergyStrategy::Splice, true, 10);
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
