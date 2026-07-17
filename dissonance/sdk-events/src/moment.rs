// SPDX-License-Identifier: AGPL-3.0-or-later
//! The V-time coordinate this boundary stamps its evidence with.

use serde::{Deserialize, Serialize};

/// A point on the single monotonic deterministic V-time axis, mirroring
/// `explorer::Moment` / `control-proto::Moment` (conventions rule 2 — defined
/// **locally**, not imported: `sdk-events` is the SDK ingress data boundary and
/// must not depend on the Explorer that consumes its normalized evidence, or the
/// two crates would form a dependency cycle once the Explorer ingests
/// [`Normalized`](crate::Normalized) evidence). The integer is one-for-one with
/// the Explorer/control-plane `Moment`, so a consumer converts with a bare
/// `Moment(m.0)`.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct Moment(pub u64);
