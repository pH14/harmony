// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crate-local signal vocabulary (task 132 M3).
//!
//! The explorer's legacy compat spine (`Sensor`/`Feature`/`FeatureSet`/
//! `ChannelId`) is physically deleted — the Differential observation plane
//! owns production observation currency. The log-template signal keeps its
//! own vocabulary here (conventions rule 2: defined locally, in the
//! consumer), with the same shapes the spine had, carrying no cross-crate
//! authority: what a feature *means* is this crate's business alone.

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

/// The features live at a given moment — the point-in-time slice
/// [`CellFnV1`](crate::CellFnV1) keys. Deterministically ordered (a
/// `BTreeSet` underneath), so no iteration order can reach a cell key.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct FeatureSet {
    features: std::collections::BTreeSet<Feature>,
}

impl FeatureSet {
    /// An empty slice.
    pub fn new() -> Self {
        Self::default()
    }

    /// The slice holding exactly one feature.
    pub fn singleton(f: Feature) -> Self {
        let mut features = std::collections::BTreeSet::new();
        features.insert(f);
        Self { features }
    }

    /// Insert a feature; returns whether it was newly present.
    pub fn insert(&mut self, f: Feature) -> bool {
        self.features.insert(f)
    }

    /// Whether the slice holds `f`.
    pub fn contains(&self, f: &Feature) -> bool {
        self.features.contains(f)
    }

    /// The features, in their canonical (sorted) order.
    pub fn iter(&self) -> impl Iterator<Item = &Feature> {
        self.features.iter()
    }

    /// The number of features in the slice.
    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// Whether the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }
}

impl FromIterator<Feature> for FeatureSet {
    fn from_iter<I: IntoIterator<Item = Feature>>(iter: I) -> Self {
        Self {
            features: iter.into_iter().collect(),
        }
    }
}
