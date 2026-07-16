// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exact, non-floating numeric normalization for numeric-guidance values.
//!
//! `docs/DISSONANCE-STRATEGY.md` and `docs/LAYERS.md` §R-L3 forbid a host `f64`
//! from ever reaching state-affecting code. A numeric-guidance report therefore
//! enters as its **original token** ([`NumericToken`], preserved verbatim from the
//! JSON) and stays **report-only** until it validates into a bounded exact
//! representation with a deterministic total order ([`BoundedNumeric`]). Nothing
//! here parses a token into `f64`; comparison is exact decimal comparison.
//!
//! [`BoundedNumeric`] is a canonical sign / significand-digits / base-10-scale
//! decimal (the strategy's "sign/coefficient/base-10-scale tuple with explicit
//! digit and exponent limits"). Non-finite, out-of-range, or over-precise input
//! fails validation ([`NumericError`]) and remains report-only evidence — it is
//! never approximately compared.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A numeric value preserved as its **original token**, exactly as it appeared in
/// the source (e.g. the JSON number text `-12.30` or `1e6`). Report-only until
/// [`NumericToken::to_bounded`] validates it into a [`BoundedNumeric`]; the raw
/// token always survives a serde round-trip so a later decoder can re-normalize
/// it under different limits.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NumericToken(String);

impl NumericToken {
    /// Wrap a raw numeric token verbatim. No validation happens here — an
    /// arbitrary string is accepted and preserved; validation is deferred to
    /// [`to_bounded`](NumericToken::to_bounded).
    pub fn new(token: impl Into<String>) -> NumericToken {
        NumericToken(token.into())
    }

    /// The original token text, byte-for-byte as ingested.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Validate and normalize into a bounded exact [`BoundedNumeric`] under
    /// `limits`, or report why it cannot be reduced. A token that fails validation
    /// stays report-only evidence — it is never coerced or approximated.
    pub fn to_bounded(&self, limits: &NumericLimits) -> Result<BoundedNumeric, NumericError> {
        BoundedNumeric::parse(&self.0, limits)
    }
}

/// Explicit digit and exponent limits bounding a [`BoundedNumeric`]. A token whose
/// significand needs more than `max_significant_digits`, or whose magnitude falls
/// outside `[min_adjusted_exponent, max_adjusted_exponent]`, is rejected rather
/// than truncated — bounded means bounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NumericLimits {
    /// Maximum number of significant decimal digits in the significand.
    pub max_significant_digits: u32,
    /// Minimum allowed adjusted exponent (power of ten of the most-significant
    /// digit); values smaller in magnitude are rejected.
    pub min_adjusted_exponent: i32,
    /// Maximum allowed adjusted exponent; values larger in magnitude are rejected.
    pub max_adjusted_exponent: i32,
}

impl NumericLimits {
    /// The default cooperative-vertical limits: a 38-digit significand (fits any
    /// `u128`/`i128` and then some) over adjusted exponents in `[-64, 64]`. Wide
    /// enough for real guidance metrics, bounded enough to stay exact.
    pub const DEFAULT: NumericLimits = NumericLimits {
        max_significant_digits: 38,
        min_adjusted_exponent: -64,
        max_adjusted_exponent: 64,
    };
}

impl Default for NumericLimits {
    fn default() -> NumericLimits {
        NumericLimits::DEFAULT
    }
}

/// Why a [`NumericToken`] could not be reduced to a [`BoundedNumeric`]. The token
/// remains report-only evidence in every case.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum NumericError {
    /// The token was not a finite decimal number (empty, `NaN`, `Infinity`, or a
    /// malformed number literal).
    #[error("numeric token `{token}` is not a finite decimal number")]
    NotFinite {
        /// The offending token.
        token: String,
    },
    /// The significand carries more significant digits than the limits allow, so
    /// an exact representation is impossible under those limits.
    #[error("numeric token `{token}` needs {digits} significant digits (limit {limit})")]
    TooManyDigits {
        /// The offending token.
        token: String,
        /// Significant digits the token requires.
        digits: u32,
        /// The configured limit.
        limit: u32,
    },
    /// The value's magnitude falls outside the allowed adjusted-exponent range.
    #[error("numeric token `{token}` has adjusted exponent {exponent} outside [{min}, {max}]")]
    ExponentOutOfRange {
        /// The offending token.
        token: String,
        /// The token's adjusted exponent.
        exponent: i32,
        /// The configured minimum.
        min: i32,
        /// The configured maximum.
        max: i32,
    },
}

/// A bounded, exact, canonical decimal with a deterministic total order.
///
/// The value is `(-1)^negative × significand × 10^scale`, where `significand` is
/// the integer spelled by [`digits`](BoundedNumeric::digits) (most-significant
/// first). The representation is **canonical**: no leading or trailing zeros in
/// `digits`, and zero is uniquely `{ negative: false, digits: "", scale: 0 }`.
/// Because the form is canonical, structural equality is value equality, and the
/// total order ([`Ord`]) matches numeric order exactly — with no `f64` anywhere in
/// the comparison path. It is a transient validation result (the persisted model
/// keeps the raw [`NumericToken`]), so it carries no serde.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundedNumeric {
    negative: bool,
    /// Significand digits, most-significant first, canonicalized (no leading or
    /// trailing zeros; empty iff the value is zero).
    digits: String,
    /// Base-10 scale: the power of ten of the least-significant stored digit.
    scale: i32,
}

impl BoundedNumeric {
    /// Whether the value is exactly zero.
    pub fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    /// Whether the value is negative (never true for zero).
    pub fn is_negative(&self) -> bool {
        self.negative
    }

    /// The canonical significand digits (most-significant first; empty iff zero).
    pub fn digits(&self) -> &str {
        &self.digits
    }

    /// The base-10 scale of the least-significant significand digit.
    pub fn scale(&self) -> i32 {
        self.scale
    }

    /// The adjusted exponent: the power of ten of the most-significant digit
    /// (`scale + len(digits) - 1`), or `0` for zero. This is the magnitude order
    /// used by the total order.
    pub fn adjusted_exponent(&self) -> i32 {
        if self.digits.is_empty() {
            0
        } else {
            self.scale + self.digits.len() as i32 - 1
        }
    }

    /// Parse and canonicalize a decimal token under `limits`. Exact: the token is
    /// split into sign / integer / fraction / exponent, its significand digits are
    /// canonicalized, and the digit/exponent limits are enforced. No `f64` is
    /// constructed at any point.
    fn parse(token: &str, limits: &NumericLimits) -> Result<BoundedNumeric, NumericError> {
        let not_finite = || NumericError::NotFinite {
            token: token.to_string(),
        };

        let bytes = token.as_bytes();
        let mut i = 0usize;
        let negative = match bytes.first() {
            Some(b'-') => {
                i += 1;
                true
            }
            Some(b'+') => {
                i += 1;
                false
            }
            _ => false,
        };

        // Integer part.
        let int_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let int_part = &token[int_start..i];

        // Optional fraction.
        let mut frac_part = "";
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            let frac_start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            frac_part = &token[frac_start..i];
        }

        // A number literal must carry at least one digit across int+fraction.
        if int_part.is_empty() && frac_part.is_empty() {
            return Err(not_finite());
        }

        // Optional exponent.
        let mut exp: i64 = 0;
        if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
            i += 1;
            let exp_neg = match bytes.get(i) {
                Some(b'-') => {
                    i += 1;
                    true
                }
                Some(b'+') => {
                    i += 1;
                    false
                }
                _ => false,
            };
            let exp_start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == exp_start {
                return Err(not_finite()); // `e` with no digits
            }
            // Bound the exponent field width so a pathological token cannot force a
            // huge parse; anything this long is out of range regardless.
            let exp_digits = &token[exp_start..i];
            if exp_digits.len() > 9 {
                return Err(NumericError::ExponentOutOfRange {
                    token: token.to_string(),
                    exponent: if exp_neg { i32::MIN } else { i32::MAX },
                    min: limits.min_adjusted_exponent,
                    max: limits.max_adjusted_exponent,
                });
            }
            let magnitude: i64 = exp_digits.parse().map_err(|_| not_finite())?;
            exp = if exp_neg { -magnitude } else { magnitude };
        }

        // Every byte must have been consumed — trailing garbage is not a number.
        if i != bytes.len() {
            return Err(not_finite());
        }

        // Assemble the raw significand digits (integer then fraction) and the scale
        // of the least-significant digit. `value = digits × 10^(exp - frac_len)`.
        let mut raw_digits = String::with_capacity(int_part.len() + frac_part.len());
        raw_digits.push_str(int_part);
        raw_digits.push_str(frac_part);
        let least_scale = exp - frac_part.len() as i64;

        BoundedNumeric::canonicalize(negative, &raw_digits, least_scale, token, limits)
    }

    /// Canonicalize raw significand `digits` (int++frac, unsigned) at `least_scale`
    /// into the no-leading/trailing-zero form, then enforce `limits`.
    fn canonicalize(
        negative: bool,
        raw_digits: &str,
        least_scale: i64,
        token: &str,
        limits: &NumericLimits,
    ) -> Result<BoundedNumeric, NumericError> {
        // Strip leading zeros (they do not change value or scale).
        let trimmed_leading = raw_digits.trim_start_matches('0');

        if trimmed_leading.is_empty() {
            // All zeros — canonical zero.
            return Ok(BoundedNumeric::zero());
        }

        // Strip trailing zeros, each one raising the scale by one.
        let trailing_zeros = trimmed_leading.len() - trimmed_leading.trim_end_matches('0').len();
        let significand = &trimmed_leading[..trimmed_leading.len() - trailing_zeros];
        let scale = least_scale + trailing_zeros as i64;

        let digit_count = significand.len() as u32;
        if digit_count > limits.max_significant_digits {
            return Err(NumericError::TooManyDigits {
                token: token.to_string(),
                digits: digit_count,
                limit: limits.max_significant_digits,
            });
        }

        // Adjusted exponent = power of ten of the most-significant digit.
        let adjusted = scale + significand.len() as i64 - 1;
        if adjusted < limits.min_adjusted_exponent as i64
            || adjusted > limits.max_adjusted_exponent as i64
        {
            return Err(NumericError::ExponentOutOfRange {
                token: token.to_string(),
                exponent: adjusted.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                min: limits.min_adjusted_exponent,
                max: limits.max_adjusted_exponent,
            });
        }

        Ok(BoundedNumeric {
            negative,
            digits: significand.to_string(),
            scale: scale as i32,
        })
    }

    /// The canonical zero.
    fn zero() -> BoundedNumeric {
        BoundedNumeric {
            negative: false,
            digits: String::new(),
            scale: 0,
        }
    }

    /// Compare magnitudes of two **non-zero** values. Larger adjusted exponent ⇒
    /// larger magnitude; on a tie, the canonical (trailing-zero-free) digit strings
    /// compare lexicographically, which orders significands correctly because the
    /// most-significant digit is first and neither has trailing zeros.
    fn cmp_magnitude(&self, other: &BoundedNumeric) -> Ordering {
        self.adjusted_exponent()
            .cmp(&other.adjusted_exponent())
            .then_with(|| self.digits.as_bytes().cmp(other.digits.as_bytes()))
    }
}

impl Ord for BoundedNumeric {
    fn cmp(&self, other: &BoundedNumeric) -> Ordering {
        match (self.is_zero(), other.is_zero()) {
            (true, true) => return Ordering::Equal,
            // Zero vs non-zero: the sign of the non-zero operand decides.
            (true, false) => {
                return if other.negative {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (false, true) => {
                return if self.negative {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            (false, false) => {}
        }
        // Both non-zero: opposite signs are decided by sign; same sign compares
        // magnitude, reversed for negatives (more magnitude ⇒ more negative).
        match (self.negative, other.negative) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.cmp_magnitude(other),
            (true, true) => self.cmp_magnitude(other).reverse(),
        }
    }
}

impl PartialOrd for BoundedNumeric {
    fn partial_cmp(&self, other: &BoundedNumeric) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
