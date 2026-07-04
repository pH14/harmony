// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tokenization and parameter pre-masking — the deterministic front of the
//! Drain fold.
//!
//! A log line is split on whitespace into tokens; a token that carries a digit
//! is pre-masked to the wildcard `<*>` (a knob, default on), because a
//! digit-bearing token is almost always a parameter (a pid, port, LSN, IP,
//! duration, UUID). Clustering then compares the *masked* token stream, while
//! parameter extraction reads the *raw* tokens at the template's wildcard
//! positions — so the original value (`54321`, `10.42.0.1`) survives into
//! `param.N` even though it was masked for clustering.

use serde::{Deserialize, Serialize};

/// The canonical wildcard rendering a masked/generalized position takes in a
/// template. Chosen to be visually distinct and stable across serialization.
pub(crate) const WILDCARD: &str = "<*>";

/// One position in a template's token stream: a literal word or a wildcard
/// (a masked or generalized parameter slot).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub(crate) enum Token {
    /// A fixed literal word (the invariant part of a template).
    Lit(String),
    /// A generalized parameter position (`<*>`).
    Wildcard,
}

impl Token {
    /// The canonical string rendering: the literal itself, or [`WILDCARD`].
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Token::Lit(s) => s,
            Token::Wildcard => WILDCARD,
        }
    }
}

/// Whether a raw token should be pre-masked: it contains at least one ASCII
/// digit. Pure and total over any `&str`.
pub(crate) fn is_parameter(raw: &str) -> bool {
    raw.bytes().any(|b| b.is_ascii_digit())
}

/// Split a line into raw whitespace tokens (borrowed from the input). The
/// denominator of every similarity comparison is `raw_tokens(line).len()`.
pub(crate) fn raw_tokens(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

/// The masked token stream a line clusters on: each raw token becomes
/// [`Token::Wildcard`] when [`is_parameter`] (and masking is enabled), else a
/// [`Token::Lit`]. Deterministic and total.
pub(crate) fn masked_tokens(line: &str, mask_digits: bool) -> Vec<Token> {
    raw_tokens(line)
        .into_iter()
        .map(|t| {
            if mask_digits && is_parameter(t) {
                Token::Wildcard
            } else {
                Token::Lit(t.to_string())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_tokens_are_parameters() {
        assert!(is_parameter("54321"));
        assert!(is_parameter("host=10.0.0.1"));
        assert!(is_parameter("pid7"));
        assert!(!is_parameter("database"));
        assert!(!is_parameter("ready"));
        assert!(!is_parameter(""));
    }

    #[test]
    fn masking_replaces_only_digit_tokens() {
        let toks = masked_tokens("connection from 10.0.0.1 port 5432", true);
        assert_eq!(
            toks,
            vec![
                Token::Lit("connection".into()),
                Token::Lit("from".into()),
                Token::Wildcard,
                Token::Lit("port".into()),
                Token::Wildcard,
            ]
        );
        // Disabling the knob keeps every token literal.
        let unmasked = masked_tokens("connection from 10.0.0.1 port 5432", false);
        assert!(unmasked.iter().all(|t| matches!(t, Token::Lit(_))));
    }

    #[test]
    fn whitespace_runs_and_empty_lines_are_total() {
        assert!(masked_tokens("", true).is_empty());
        assert!(masked_tokens("   \t  ", true).is_empty());
        assert_eq!(masked_tokens("  a   b ", true).len(), 2);
    }

    #[test]
    fn wildcard_renders_canonically() {
        assert_eq!(Token::Wildcard.as_str(), WILDCARD);
        assert_eq!(Token::Lit("x".into()).as_str(), "x");
    }
}
