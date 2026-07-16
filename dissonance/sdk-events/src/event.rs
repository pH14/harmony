// SPDX-License-Identifier: AGPL-3.0-or-later
//! The normalized, ordered **`SdkEvent`** and the [`Normalized`] decode bundle.
//!
//! An `SdkEvent` is one decoded, timestamped observation. It carries the four
//! roles the strategy keeps separate — *source provenance* ([`SdkEvent::source`]),
//! *observation identity* ([`SdkEvent::id`]), *site provenance*
//! ([`SdkEvent::site`], for assertions), and the *value* ([`SdkEvent::payload`]) —
//! plus its ordering coordinates and the recoverable raw record. It does **not**
//! carry a cell, a reduction, or a verdict; those are above this boundary.

use explorer::Moment;
use serde::{Deserialize, Serialize};

use crate::numeric::NumericToken;
use crate::schema::{ObservationId, Raw, SdkSchema, SourceFormat, UpdateOp};

/// The result of decoding one ingress stream: the normalized schema plus the
/// ordered events. The schema's entries and the events' ordinals are canonical and
/// identical across platforms.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Normalized {
    /// The normalized schema derived from (or declared by) the stream.
    pub schema: SdkSchema,
    /// The ordered events, in persisted (source-ordinal) order.
    pub events: Vec<SdkEvent>,
}

/// The kind of Antithesis assertion an [`Payload::Assertion`] evidences. These are
/// the verbs of the adopted Antithesis surface (`docs/LAYERS.md` §R-L3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AssertType {
    /// `always` — the condition must hold at every evaluation.
    Always,
    /// `sometimes` — the condition must hold at some evaluation.
    Sometimes,
    /// `reachable` — the point must be reached.
    Reachable,
    /// `unreachable` — the point must never be reached.
    Unreachable,
}

/// An assertion **site** — provenance and coverage, kept separate from the
/// aggregated property identity. Multiple sites may contribute to one property
/// (`docs/DISSONANCE-STRATEGY.md`: "the assertion message identifies the property
/// and multiple sites may contribute to it; site identity remains provenance and
/// coverage").
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SiteId {
    /// The source's per-site assertion `id`, if present — site metadata, not the
    /// property identity (the message is the property; see [`SdkEvent::id`]).
    pub id: Option<String>,
    /// The source file the assertion is in.
    pub file: String,
    /// The enclosing function/class path.
    pub function: String,
    /// The 1-based line of the assertion.
    pub begin_line: u32,
    /// The 1-based column of the assertion.
    pub begin_column: u32,
}

/// The normalized value an event carries. Occurrence and state payloads are kept
/// distinct so a downstream reducer never mistakes a one-shot hit for persistent
/// state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Payload {
    /// An assertion evaluation — occurrence/property evidence. `condition` is the
    /// asserted predicate's value at this evaluation, when the source reported it.
    Assertion {
        /// The assertion verb, if known (always known for Antithesis JSON; `None`
        /// for a binary firing at an undeclared coordinate).
        assert_type: Option<AssertType>,
        /// The evaluated condition, if reported.
        condition: Option<bool>,
    },
    /// A state-register update. `op` is the base reduction the value participates
    /// in; `value` is the reported integer (the initial cooperative-vertical shape).
    State {
        /// The base update operation.
        op: UpdateOp,
        /// The reported value.
        value: u64,
    },
    /// A numeric-guidance report — a monotone extremum only (never arbitrary `set`
    /// state, because the SDK may filter reports to new watermarks). `op` is `Max`
    /// or `Min`; `token` is the original numeric token, report-only until it
    /// normalizes into a bounded exact representation.
    Guidance {
        /// The extremum direction (`Max` for `maximize`, `Min` otherwise).
        op: UpdateOp,
        /// The reported extremum as its original token, if the record carried a
        /// scalar metric; `None` when only non-scalar operands were present (the
        /// operands survive in [`SdkEvent::raw`]).
        token: Option<NumericToken>,
    },
    /// A buggify decision outcome (occurrence): whether the fault fired.
    Buggify {
        /// Whether the buggify point fired.
        fired: bool,
    },
    /// A lifecycle point (e.g. `setup_complete`).
    Lifecycle {
        /// The lifecycle point name.
        name: String,
    },
    /// An unrecognized or opaque record — nothing normalized; the raw bytes in
    /// [`SdkEvent::raw`] are the whole of it.
    Unknown,
}

/// One decoded, timestamped observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkEvent {
    /// The V-time anchor this event surfaced at. Per the open issue `hm-ynt`, an
    /// SDK event `Moment` is a V-time-anchor **lower bound**, not necessarily the
    /// exact emission `Moment`; this boundary carries it through faithfully and
    /// neither tightens nor loosens that contract.
    pub moment: Moment,
    /// The rollout-local **source ordinal**: the event's persisted vector position.
    /// Contractual within this source (per [`OrderingScope`](crate::OrderingScope));
    /// cross-source sequencing needs a shared machine-event ordinal this boundary
    /// does not have.
    pub ordinal: u64,
    /// Which ingress format produced this event.
    pub source: SourceFormat,
    /// The stable observation identity.
    pub id: ObservationId,
    /// The assertion site (provenance/coverage), separate from the property
    /// identity; `None` for non-assertion events.
    pub site: Option<SiteId>,
    /// The normalized value.
    pub payload: Payload,
    /// The original source record, preserved verbatim for audit/migration.
    pub raw: Raw,
}
