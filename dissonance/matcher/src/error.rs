// SPDX-License-Identifier: AGPL-3.0-or-later
//! The typed config error. Malformed config is **never** a panic on untrusted
//! input (conventions rule 4): every malformed class parses to one of these
//! variants.

/// An error parsing or validating a signal-set config.
///
/// The task-66 acceptance gate enumerates the malformed classes each of which
/// must yield a *typed* error, not a panic:
/// - **bad type / bad JSON** → [`MatchError::Parse`] (the underlying
///   `serde_json` error: a non-string attr value, a number where an object is
///   expected, truncated JSON, …);
/// - **unknown role** → [`MatchError::UnknownRole`];
/// - **duplicate name** → [`MatchError::DuplicateName`];
/// - **unknown `during` predicate** → [`MatchError::UnknownDuring`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MatchError {
    /// The JSON was syntactically invalid, or a field had the wrong type (e.g.
    /// an attribute value that was not a string). Carries the `serde_json`
    /// diagnostic verbatim.
    #[error("invalid signal config JSON: {0}")]
    Parse(#[from] serde_json::Error),

    /// A signal declared a `role` that is not one of `sometimes` / `never` /
    /// `cell` / `state_max`.
    #[error("unknown role {0:?} (expected sometimes | never | cell | state_max)")]
    UnknownRole(String),

    /// A signal declared a `during` predicate the DSL does not (yet) ship. v1
    /// ships exactly `no_faults`; the predicate vocabulary stays extensible.
    #[error("unknown during predicate {0:?} (expected no_faults)")]
    UnknownDuring(String),

    /// Two signals shared a `name`. The declared set is the catalog, so names
    /// must be unique for never-fired detection to be well-defined.
    #[error("duplicate signal name {0:?}")]
    DuplicateName(String),

    /// More signals than the channel space can address. Each signal's channel
    /// is the rank of its name in `[0, u16::MAX]`, so a set larger than
    /// `u16::MAX + 1` (65 536) would wrap ranks and collide two signals onto one
    /// channel — rejected fail-closed at construction rather than silently
    /// merging their outputs.
    #[error("too many signals: {0} exceeds the channel capacity of 65536 (u16::MAX + 1)")]
    TooManySignals(usize),
}
