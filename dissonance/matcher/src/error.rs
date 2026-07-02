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
/// - **unknown `during` predicate** → [`MatchError::UnknownDuring`];
/// - **`state_max` without `attr_max`** → [`MatchError::StateMaxWithoutAttrMax`].
///
/// Channel-space errors ([`ReservedChannelBase`](MatchError::ReservedChannelBase),
/// [`ChannelSpaceExhausted`](MatchError::ChannelSpaceExhausted)) surface at
/// [`MatchSensor`](crate::MatchSensor) construction, where the campaign's
/// channel base is known.
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

    /// A `state_max` signal declared no `attr_max`. Such a signal matches
    /// records, emits no feature (there is no register to fold), yet reports
    /// fired — a vacuous config. Rejected at validation: the `state_max` role
    /// requires an `attr_max`.
    #[error("state_max signal {0:?} declares no attr_max (the register to fold)")]
    StateMaxWithoutAttrMax(String),

    /// A [`MatchSensor`](crate::MatchSensor) was constructed with channel base
    /// `0`, which is reserved for the coverage channel (spine
    /// `COVERAGE_CHANNEL`). A matcher must allocate its channels at base ≥ 1 so
    /// its features never collide with coverage's in the archive's `Feature`
    /// space.
    #[error("channel base 0 is reserved for coverage; use a base >= 1")]
    ReservedChannelBase,

    /// A [`MatchSensor`](crate::MatchSensor)'s signal set does not fit the
    /// channel space above `base`: `base + count` exceeds `u16::MAX` (channels
    /// are `base + name-rank`). Rejected fail-closed rather than wrapping a rank
    /// and merging two signals onto one channel.
    #[error("channel space exhausted: base {base} + {count} signals exceeds u16::MAX")]
    ChannelSpaceExhausted {
        /// The channel base the sensor was given.
        base: u16,
        /// The number of signals in the set.
        count: usize,
    },
}
