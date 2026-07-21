// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crate-local signal vocabulary (task 132 M3).
//!
//! The explorer's legacy compat spine (`Sensor`/`Feature`/`FeatureSet`/
//! `ChannelId`) is physically deleted — the Differential observation plane
//! owns production observation currency. The matcher keeps its own signal
//! vocabulary here (conventions rule 2: defined locally, in the consumer),
//! with the same shapes the spine had, carrying no cross-crate authority:
//! what a feature *means* is this crate's business alone.

use serde::{Deserialize, Serialize};

/// A stable channel identifier: which signal tier/plugin a [`Feature`] came
/// from. Channel numbering is a campaign convention; only stability is
/// required.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct ChannelId(pub u16);

/// A stable feature identifier within a channel (here: a template species
/// id, minted first-seen and stable across the run sequence).
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct FeatureId(pub u64);

/// One observed signal: a stable `(channel, id)` pair.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct Feature {
    /// The signal channel this feature belongs to.
    pub channel: ChannelId,
    /// The stable feature identity within the channel.
    pub id: FeatureId,
}
