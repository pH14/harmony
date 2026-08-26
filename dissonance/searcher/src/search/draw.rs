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

/// How many actions one job appends to its parent's input.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SuffixShape {
    /// One action, or two at one-in-four odds.
    #[default]
    OneOrTwo,
}

/// The recorded identifier of a suffix shape.
#[must_use]
pub fn suffix_shape_identifier(shape: SuffixShape) -> &'static str {
    match shape {
        SuffixShape::OneOrTwo => SUFFIX_ONE_OR_TWO_IDENTIFIER,
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
    mutation_seed: u64,
    mut biased: B,
    mut alphabet: U,
) -> Result<Vec<A>, Box<dyn Error>>
where
    B: FnMut(&mut RomuDuoJrRand) -> Result<Option<A>, Box<dyn Error>>,
    U: FnMut(&mut RomuDuoJrRand) -> Result<A, Box<dyn Error>>,
{
    let mut rand = RomuDuoJrRand::with_seed(mutation_seed);
    let length = match shape {
        SuffixShape::OneOrTwo => {
            if rand.below(NonZeroUsize::new(4).ok_or("invalid suffix odds")?) == 0 {
                2
            } else {
                1
            }
        }
    };
    let mut suffix = Vec::with_capacity(length);
    for _ in 0..length {
        let take_biased = mixture == DrawMixture::BiasedHalf
            && rand.below(NonZeroUsize::new(2).ok_or("invalid mixture odds")?) == 0;
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
        DrawMixture, SuffixShape, draw_suffix, suffix_shape_from_identifier,
        suffix_shape_identifier,
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
}
