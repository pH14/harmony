// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bounded replacement of contiguous regions in replayable step sequences.

use std::{error::Error, fmt, num::NonZeroUsize};

use serde::{Deserialize, Serialize};

/// Maximum registered replacement lengths accepted by one configuration.
pub const MAX_REPLACEMENT_LENGTHS: usize = 16;
/// Maximum donor sequences accepted by one replacement.
pub const MAX_REPLACEMENT_DONORS: usize = 131_072;
/// Compiled ceiling on a sequence accepted by this generic mechanism.
pub const MAX_EDIT_SEQUENCE_STEPS: usize = 1_048_576;

/// Registered bounds and replacement lengths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReplacementParameters {
    /// Strictly increasing allowed replacement lengths.
    pub lengths: Vec<NonZeroUsize>,
    /// Maximum steps accepted in an input or donor sequence.
    pub maximum_input_steps: NonZeroUsize,
}

impl ReplacementParameters {
    /// Validate allocation bounds and the canonical ordering of lengths.
    ///
    /// # Errors
    ///
    /// Returns an error when the length set is empty, excessive, unordered,
    /// duplicated, or outside the configured and compiled sequence bounds.
    pub fn validate(&self) -> Result<(), SequenceEditError> {
        if self.lengths.is_empty() {
            return Err(SequenceEditError::InvalidParameters(
                "replacement lengths must be nonempty",
            ));
        }
        if self.lengths.len() > MAX_REPLACEMENT_LENGTHS {
            return Err(SequenceEditError::InvalidParameters(
                "too many replacement lengths",
            ));
        }
        if self.maximum_input_steps.get() > MAX_EDIT_SEQUENCE_STEPS {
            return Err(SequenceEditError::InvalidParameters(
                "maximum input steps exceeds the compiled bound",
            ));
        }
        let mut previous = 0_usize;
        for length in &self.lengths {
            if length.get() <= previous {
                return Err(SequenceEditError::InvalidParameters(
                    "replacement lengths must be strictly increasing",
                ));
            }
            if length.get() > self.maximum_input_steps.get() {
                return Err(SequenceEditError::InvalidParameters(
                    "replacement length exceeds maximum input steps",
                ));
            }
            previous = length.get();
        }
        Ok(())
    }
}

/// One reproducible same-length replacement recipe.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplacementRecipe {
    /// Index into the registered replacement lengths.
    pub length_index: usize,
    /// First replaced step in the input.
    pub input_start: usize,
    /// Index of the donor sequence.
    pub donor_index: usize,
    /// First copied step in the donor sequence.
    pub donor_start: usize,
}

/// Apply a checked same-length replacement without interpreting any step.
///
/// The result is exactly the input prefix before the selected window, the
/// selected donor window, and the untouched input suffix after the replaced
/// window. Its length therefore always equals the input length.
///
/// # Errors
///
/// Returns an error for invalid parameters, an excessive sequence or donor
/// collection, an unknown recipe index, arithmetic overflow, or an out-of-range
/// input or donor window.
pub fn apply_replacement<Step: Clone>(
    input: &[Step],
    donors: &[&[Step]],
    recipe: &ReplacementRecipe,
    parameters: &ReplacementParameters,
) -> Result<Vec<Step>, SequenceEditError> {
    parameters.validate()?;
    validate_sequence_len(input.len(), parameters.maximum_input_steps.get(), None)?;
    if donors.len() > MAX_REPLACEMENT_DONORS {
        return Err(SequenceEditError::TooManyDonors {
            actual: donors.len(),
            maximum: MAX_REPLACEMENT_DONORS,
        });
    }
    for (index, donor) in donors.iter().enumerate() {
        validate_sequence_len(
            donor.len(),
            parameters.maximum_input_steps.get(),
            Some(index),
        )?;
    }

    let length = parameters
        .lengths
        .get(recipe.length_index)
        .ok_or(SequenceEditError::UnknownLengthIndex(recipe.length_index))?
        .get();
    let input_end = recipe
        .input_start
        .checked_add(length)
        .ok_or(SequenceEditError::RangeOverflow("input replacement"))?;
    let donor = donors
        .get(recipe.donor_index)
        .ok_or(SequenceEditError::UnknownDonorIndex(recipe.donor_index))?;
    let donor_end = recipe
        .donor_start
        .checked_add(length)
        .ok_or(SequenceEditError::RangeOverflow("donor replacement"))?;

    let input_prefix = input
        .get(..recipe.input_start)
        .ok_or(SequenceEditError::InputRangeOutOfBounds)?;
    let input_suffix = input
        .get(input_end..)
        .ok_or(SequenceEditError::InputRangeOutOfBounds)?;
    let donor_window = donor
        .get(recipe.donor_start..donor_end)
        .ok_or(SequenceEditError::DonorRangeOutOfBounds)?;

    let mut replaced = Vec::with_capacity(input.len());
    replaced.extend_from_slice(input_prefix);
    replaced.extend_from_slice(donor_window);
    replaced.extend_from_slice(input_suffix);
    Ok(replaced)
}

/// Failure to configure or apply a sequence replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SequenceEditError {
    /// A registered bound is vacuous or inconsistent.
    InvalidParameters(&'static str),
    /// An input sequence exceeds its configured bound.
    InputTooLong {
        /// Observed input length.
        actual: usize,
        /// Registered maximum.
        maximum: usize,
    },
    /// A donor sequence exceeds its configured bound.
    DonorTooLong {
        /// Donor index.
        index: usize,
        /// Observed donor length.
        actual: usize,
        /// Registered maximum.
        maximum: usize,
    },
    /// The donor collection exceeds its compiled bound.
    TooManyDonors {
        /// Observed donor count.
        actual: usize,
        /// Compiled maximum.
        maximum: usize,
    },
    /// A recipe names an unknown registered length.
    UnknownLengthIndex(usize),
    /// A recipe names an unknown donor.
    UnknownDonorIndex(usize),
    /// A range endpoint overflowed.
    RangeOverflow(&'static str),
    /// A recipe's input range is outside the input.
    InputRangeOutOfBounds,
    /// A recipe's donor range is outside the donor.
    DonorRangeOutOfBounds,
}

impl fmt::Display for SequenceEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameters(message) => formatter.write_str(message),
            Self::InputTooLong { actual, maximum } => {
                write!(formatter, "input has {actual} steps; maximum is {maximum}")
            }
            Self::DonorTooLong {
                index,
                actual,
                maximum,
            } => write!(
                formatter,
                "donor {index} has {actual} steps; maximum is {maximum}"
            ),
            Self::TooManyDonors { actual, maximum } => {
                write!(formatter, "received {actual} donors; maximum is {maximum}")
            }
            Self::UnknownLengthIndex(index) => {
                write!(formatter, "replacement length index {index} is unknown")
            }
            Self::UnknownDonorIndex(index) => {
                write!(formatter, "replacement donor index {index} is unknown")
            }
            Self::RangeOverflow(range) => write!(formatter, "{range} range overflowed"),
            Self::InputRangeOutOfBounds => {
                formatter.write_str("replacement input range is out of bounds")
            }
            Self::DonorRangeOutOfBounds => {
                formatter.write_str("replacement donor range is out of bounds")
            }
        }
    }
}

impl Error for SequenceEditError {}

fn validate_sequence_len(
    actual: usize,
    maximum: usize,
    donor_index: Option<usize>,
) -> Result<(), SequenceEditError> {
    if actual <= maximum {
        return Ok(());
    }
    match donor_index {
        Some(index) => Err(SequenceEditError::DonorTooLong {
            index,
            actual,
            maximum,
        }),
        None => Err(SequenceEditError::InputTooLong { actual, maximum }),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use proptest::{collection::vec, prelude::*, test_runner::Config as ProptestConfig};

    use super::{
        MAX_EDIT_SEQUENCE_STEPS, MAX_REPLACEMENT_DONORS, MAX_REPLACEMENT_LENGTHS,
        ReplacementParameters, ReplacementRecipe, SequenceEditError, apply_replacement,
    };

    fn nonzero(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test values are nonzero")
    }

    fn parameters(lengths: &[usize], maximum_input_steps: usize) -> ReplacementParameters {
        ReplacementParameters {
            lengths: lengths.iter().copied().map(nonzero).collect(),
            maximum_input_steps: nonzero(maximum_input_steps),
        }
    }

    fn recipe(length_index: usize) -> ReplacementRecipe {
        ReplacementRecipe {
            length_index,
            input_start: 0,
            donor_index: 0,
            donor_start: 0,
        }
    }

    #[test]
    fn replacement_is_exact_and_preserves_length() {
        let input = [0, 1, 2, 3, 4, 5];
        let donor = [10, 11, 12, 13];
        let replaced = apply_replacement(
            &input,
            &[&donor],
            &ReplacementRecipe {
                length_index: 0,
                input_start: 2,
                donor_index: 0,
                donor_start: 1,
            },
            &parameters(&[2], 8),
        )
        .expect("valid replacement");
        assert_eq!(replaced, [0, 1, 11, 12, 4, 5]);
        assert_eq!(replaced.len(), input.len());
    }

    #[test]
    fn parameter_validation_enforces_every_hard_bound() {
        assert!(parameters(&[], 8).validate().is_err());
        assert!(parameters(&[2, 2], 8).validate().is_err());
        assert!(parameters(&[2, 1], 8).validate().is_err());
        assert!(parameters(&[9], 8).validate().is_err());
        assert!(
            parameters(
                &(1..=MAX_REPLACEMENT_LENGTHS + 1).collect::<Vec<_>>(),
                MAX_REPLACEMENT_LENGTHS + 1,
            )
            .validate()
            .is_err()
        );
        assert!(
            parameters(&[1], MAX_EDIT_SEQUENCE_STEPS + 1)
                .validate()
                .is_err()
        );
    }

    #[test]
    fn malformed_recipe_indices_and_ranges_return_errors() {
        let configured = parameters(&[2], 8);
        let input = [0, 1, 2, 3];
        let donor = [4, 5, 6, 7];
        assert_eq!(
            apply_replacement(&input, &[&donor], &recipe(1), &configured),
            Err(SequenceEditError::UnknownLengthIndex(1))
        );
        let missing_donor = ReplacementRecipe {
            donor_index: 1,
            ..recipe(0)
        };
        assert_eq!(
            apply_replacement(&input, &[&donor], &missing_donor, &configured),
            Err(SequenceEditError::UnknownDonorIndex(1))
        );
        let overflowing_input = ReplacementRecipe {
            input_start: usize::MAX,
            ..recipe(0)
        };
        assert!(matches!(
            apply_replacement(&input, &[&donor], &overflowing_input, &configured),
            Err(SequenceEditError::RangeOverflow("input replacement"))
        ));
        let overflowing_donor = ReplacementRecipe {
            donor_start: usize::MAX,
            ..recipe(0)
        };
        assert!(matches!(
            apply_replacement(&input, &[&donor], &overflowing_donor, &configured),
            Err(SequenceEditError::RangeOverflow("donor replacement"))
        ));
        let late_input = ReplacementRecipe {
            input_start: 3,
            ..recipe(0)
        };
        assert_eq!(
            apply_replacement(&input, &[&donor], &late_input, &configured),
            Err(SequenceEditError::InputRangeOutOfBounds)
        );
        let late_donor = ReplacementRecipe {
            donor_start: 3,
            ..recipe(0)
        };
        assert_eq!(
            apply_replacement(&input, &[&donor], &late_donor, &configured),
            Err(SequenceEditError::DonorRangeOutOfBounds)
        );
    }

    #[test]
    fn input_donor_and_donor_count_bounds_are_checked_before_slicing() {
        let configured = parameters(&[1], 4);
        let donor = [1_u8];
        assert!(matches!(
            apply_replacement(&[0_u8; 5], &[&donor], &recipe(0), &configured),
            Err(SequenceEditError::InputTooLong {
                actual: 5,
                maximum: 4
            })
        ));
        assert!(matches!(
            apply_replacement(&[0_u8], &[&[1_u8; 5]], &recipe(0), &configured),
            Err(SequenceEditError::DonorTooLong {
                index: 0,
                actual: 5,
                maximum: 4
            })
        ));
        let donors = vec![donor.as_slice(); MAX_REPLACEMENT_DONORS + 1];
        assert!(matches!(
            apply_replacement(&[0_u8], &donors, &recipe(0), &configured),
            Err(SequenceEditError::TooManyDonors {
                actual,
                maximum: MAX_REPLACEMENT_DONORS
            }) if actual == MAX_REPLACEMENT_DONORS + 1
        ));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn checked_replacement_matches_the_sequence_model(
            input in vec(any::<u8>(), 1..129),
            donor in vec(any::<u8>(), 1..129),
            raw_length in 1_usize..129,
            raw_input_start in any::<usize>(),
            raw_donor_start in any::<usize>(),
        ) {
            let length = raw_length.min(input.len()).min(donor.len()).max(1);
            let input_positions = input.len() - length + 1;
            let donor_positions = donor.len() - length + 1;
            let input_start = raw_input_start % input_positions;
            let donor_start = raw_donor_start % donor_positions;
            let configured = parameters(&[length], 128);
            let generated = apply_replacement(
                &input,
                &[donor.as_slice()],
                &ReplacementRecipe {
                    length_index: 0,
                    input_start,
                    donor_index: 0,
                    donor_start,
                },
                &configured,
            ).expect("generated valid replacement");
            let mut expected = input.clone();
            expected[input_start..input_start + length]
                .clone_from_slice(&donor[donor_start..donor_start + length]);
            prop_assert_eq!(generated, expected);
        }

        #[test]
        fn arbitrary_recipe_indices_never_panic(
            input in vec(any::<u8>(), 0..65),
            donor in vec(any::<u8>(), 0..65),
            length_index in any::<usize>(),
            input_start in any::<usize>(),
            donor_index in any::<usize>(),
            donor_start in any::<usize>(),
        ) {
            let configured = parameters(&[1, 2, 4], 64);
            let _ = apply_replacement(
                &input,
                &[donor.as_slice()],
                &ReplacementRecipe {
                    length_index,
                    input_start,
                    donor_index,
                    donor_start,
                },
                &configured,
            );
        }

        #[test]
        fn oversized_input_or_donor_is_rejected_before_recipe_use(
            input in vec(any::<u8>(), 0..65),
            donor in vec(any::<u8>(), 0..65),
        ) {
            let configured = parameters(&[1], 32);
            let result = apply_replacement(
                &input,
                &[donor.as_slice()],
                &ReplacementRecipe {
                    length_index: usize::MAX,
                    input_start: usize::MAX,
                    donor_index: usize::MAX,
                    donor_start: usize::MAX,
                },
                &configured,
            );
            if input.len() > 32 {
                prop_assert_eq!(
                    result,
                    Err(SequenceEditError::InputTooLong {
                        actual: input.len(),
                        maximum: 32,
                    })
                );
            } else if donor.len() > 32 {
                prop_assert_eq!(
                    result,
                    Err(SequenceEditError::DonorTooLong {
                        index: 0,
                        actual: donor.len(),
                        maximum: 32,
                    })
                );
            }
        }
    }
}
