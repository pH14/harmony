// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed decode/normalization errors.
//!
//! The boundary is **panic-free on untrusted input** (conventions rule 4): a
//! hostile or garbled stream never crashes the decoder. Unrecognized data is
//! *preserved raw*, not an error — an unknown namespace, an unknown JSON key, or
//! an opaque payload becomes a raw-carrying `Unknown` event. A **structural
//! contradiction** the boundary cannot normalize is a typed error instead:
//! malformed length prefixes, an identity carrying two different base operations
//! or value shapes, a flip between occurrence and state, or an unrecognized
//! declaration byte. Numeric tokens that do not fit a bounded exact representation
//! are *not* errors here — they stay report-only evidence (see
//! [`crate::numeric`]).

use thiserror::Error;

use crate::schema::{Classification, ObservationId, UpdateOp, ValueShape};

/// A typed ingress error. Every decoder returns `Result<_, SdkError>` and never
/// panics on adversarial input.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SdkError {
    /// A length prefix or fixed field claimed more bytes than the frame carried.
    #[error("malformed length in {context}: need {needed} bytes, {available} available")]
    MalformedLength {
        /// Where in the frame the overrun occurred.
        context: &'static str,
        /// Bytes the field claimed.
        needed: usize,
        /// Bytes actually available.
        available: usize,
    },

    /// One state identity carried two different base update operations — the
    /// binary-v1 "mixed operations for one identity are malformed evidence" rule,
    /// and the v2/guidance "declared op must match" rule.
    #[error("identity {id:?} carried conflicting base operations: {first:?} then {second:?}")]
    MixedOperations {
        /// The conflicted identity.
        id: ObservationId,
        /// The first operation seen.
        first: UpdateOp,
        /// The conflicting later operation.
        second: UpdateOp,
    },

    /// One identity carried two incompatible value shapes.
    #[error("identity {id:?} carried incompatible value shapes: {first:?} then {second:?}")]
    IncompatibleShapes {
        /// The conflicted identity.
        id: ObservationId,
        /// The first shape seen.
        first: ValueShape,
        /// The conflicting later shape.
        second: ValueShape,
    },

    /// One identity was classified both as an occurrence and as state.
    #[error("identity {id:?} classified as both {first:?} and {second:?}")]
    ClassificationConflict {
        /// The conflicted identity.
        id: ObservationId,
        /// The first classification seen.
        first: Classification,
        /// The conflicting later classification.
        second: Classification,
    },

    /// A declaration carried a byte in a fixed enumerated field that no version of
    /// the format defines.
    #[error("unrecognized {field} byte {value} in declaration")]
    UnknownDeclarationByte {
        /// The field the byte was in (`classification`, `value_shape`, `base_op`,
        /// `expectation`).
        field: &'static str,
        /// The unrecognized byte.
        value: u8,
    },
}
