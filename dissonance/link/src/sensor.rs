// SPDX-License-Identifier: AGPL-3.0-or-later
//! The link [`Sensor`]: a decoded event stream → timestamped [`Feature`]s.
//!
//! An `assert_sometimes`/`assert_reachable` **hit** or an IJON **state-register**
//! report becomes a `(Moment, Feature)` in the feature stream. The features are
//! **timestamped** (task 64: a run passes through many interesting states, each
//! stamped with the moment it was observed), not a terminal set.
//!
//! Admission still requires a novel `(cell, Moment)` at the spine
//! [`Archive`](explorer::Archive) (task 64 semantics), so per-hit checkpoint
//! candidacy requires the campaign's `CellFn` config to include the link
//! channels — the sensor only *produces* the features; the archive decides
//! novelty.

use explorer::{ChannelId, Feature, FeatureId, Moment, RunTrace, Sensor};

use crate::decode::{KIND_ASSERT_HIT, KIND_STATE, attr_u64};

/// The channel link **assertion hits** are filed under. `0` is coverage, `1` the
/// scrape base; the link tier takes a clearly-separated pair (16/17).
pub const LINK_ASSERT_CHANNEL: ChannelId = ChannelId(16);
/// The channel link **state-register** changes are filed under.
pub const LINK_STATE_CHANNEL: ChannelId = ChannelId(17);

/// The link-tier sensor. Stateless (pure per run): same trace, same stream.
#[derive(Clone, Debug, Default)]
pub struct LinkSensor;

impl LinkSensor {
    /// The link sensor (stateless).
    pub fn new() -> LinkSensor {
        LinkSensor
    }
}

impl Sensor for LinkSensor {
    fn observe(&self, t: &RunTrace) -> Vec<(Moment, Feature)> {
        let mut out = Vec::new();
        for (at, ev) in &t.events {
            match ev.kind.as_str() {
                // An assertion hit: one feature per distinct point (a point that
                // hits many times dedups to one cell in the FeatureSet).
                KIND_ASSERT_HIT => {
                    if let Some(point) = attr_u64(ev, "point") {
                        out.push((
                            *at,
                            Feature {
                                channel: LINK_ASSERT_CHANNEL,
                                id: FeatureId(point),
                            },
                        ));
                    }
                }
                // A state-register report: the feature encodes the (reg, value)
                // pair so a new value is a new cell (IJON max-novelty).
                KIND_STATE => {
                    if let (Some(reg), Some(value)) = (attr_u64(ev, "reg"), attr_u64(ev, "value")) {
                        out.push((
                            *at,
                            Feature {
                                channel: LINK_STATE_CHANNEL,
                                id: FeatureId(pack_state(reg, value)),
                            },
                        ));
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// Pack a `(reg, value)` into a stable 64-bit feature id: the low 16 bits of the
/// register in the top word, the low 48 bits of the value below. Distinct values
/// of a register are distinct features (the novelty the IJON annotation wants).
/// The truncation to 16-bit registers and 48-bit values only ever *collapses*
/// two features into one cell — it never invents novelty, so it is a coverage
/// trade-off, never a correctness bug; both are far beyond any realistic
/// register count or state magnitude.
fn pack_state(reg: u64, value: u64) -> u64 {
    ((reg & 0xFFFF) << 48) | (value & 0x0000_FFFF_FFFF_FFFF)
}
