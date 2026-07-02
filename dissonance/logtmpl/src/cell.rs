// SPDX-License-Identifier: AGPL-3.0-or-later
//! CellFn v1 — the first multi-channel, point-in-time, **bounded** cell
//! function, and the first serious answer to hard problem #1 (the cell
//! abstraction).
//!
//! [`CellFnV1::key`] composes, in a fixed channel order, a length-prefixed byte
//! encoding of:
//!
//! 1. **species-progress** — the (log2-bucketed, by default) count of distinct
//!    template species present in the slice;
//! 2. **last-new-species** — the id of the most recently first-seen template,
//!    folded `mod k`. Template ids are minted in first-seen order, so the
//!    largest template id present *is* the most recently first-seen one — no
//!    ordering state is needed;
//! 3. **each matcher `cell`-role channel** — the latest value-id observed on
//!    that channel, folded `mod k` (the reified state SGFuzz says to harvest:
//!    pod phase, recovery state).
//!
//! **Coverage is excluded by construction** (the EXPLORATION ruling: coverage is
//! a *terminal* signal, never blended into along-timeline cell keys). CellFn v1
//! takes no coverage input and none is addable through its config.
//!
//! The cell function is the archive's **only** size bound (too fine explodes the
//! archive on one trajectory, too coarse hides progress); the cardinality
//! knobs — per-channel enable, quantization, fold modulus — are therefore
//! config-visible, not constants, so task 69's correlation harness can tune them.
//!
//! ## The point-in-time slice contract
//!
//! `feats` is the feature slice *live at* `at`: the accumulating template
//! channel carries every species seen at ≤ `at` (channels 1–2 read its count and
//! max id), and each state channel carries its current value (channel 3 reads
//! it). The key is a pure function of that slice — moment-blind, like the
//! spine's `IdentityCells` — so identical slices key identically wherever they
//! recur, which is what makes the cell count bounded rather than per-moment.

use serde::{Deserialize, Serialize};

use explorer::{CellFn, CellKey, ChannelId, FeatureSet, Moment};

use crate::sensor::TEMPLATE_CHANNEL;

/// The default fold modulus `k`: the counters `last-new-species` and each state
/// channel fold `mod k` to cap their cardinality contribution.
pub const DEFAULT_FOLD_K: u64 = 64;

/// How a counter channel is quantized before it enters the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Quant {
    /// Log2 bucket: `0 → 0`, else `floor(log2(n)) + 1` (the significant-bit
    /// count). Coarsens a monotone counter so progress stays visible without
    /// exploding the archive. The default.
    #[default]
    Log2,
    /// Identity: the raw count. Finer — a knob for campaigns that want it.
    Identity,
}

impl Quant {
    /// Apply the quantization to a raw counter value.
    fn apply(self, n: u64) -> u64 {
        match self {
            Quant::Log2 => log2_bucket(n),
            Quant::Identity => n,
        }
    }
}

/// The log2 bucket of a count: `0` for `0`, else `floor(log2(n)) + 1`
/// (equivalently the number of significant bits). Groups `1`, `2..3`, `4..7`,
/// `8..15`, … into buckets `1, 2, 3, 4, …`.
pub fn log2_bucket(n: u64) -> u64 {
    if n == 0 {
        0
    } else {
        (u64::BITS - n.leading_zeros()) as u64
    }
}

/// The CellFn v1 configuration — the mandatory cardinality-control knobs, in a
/// `serde` config so task 69 can tune them without a recompile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellConfig {
    /// The channel the template-species features arrive on (channels 1–2 read
    /// it). Defaults to [`TEMPLATE_CHANNEL`].
    pub template_channel: ChannelId,
    /// Enable channel 1 (species-progress).
    pub species_progress: bool,
    /// How channel 1's distinct-species count is quantized. Default [`Quant::Log2`].
    pub species_quant: Quant,
    /// Enable channel 2 (last-new-species).
    pub last_new_species: bool,
    /// The matcher `cell`-role channels to compose, in fixed order (channel 3+).
    /// Empty by default — a campaign wires in the pod-phase / recovery-state
    /// channels it wants harvested.
    pub cell_channels: Vec<ChannelId>,
    /// The fold modulus `k` for channel 2 and every state channel. A `0` is
    /// treated as "no fold" so a hand-built config cannot divide by zero.
    pub fold_k: u64,
}

impl Default for CellConfig {
    fn default() -> Self {
        Self {
            template_channel: TEMPLATE_CHANNEL,
            species_progress: true,
            species_quant: Quant::Log2,
            last_new_species: true,
            cell_channels: Vec::new(),
            fold_k: DEFAULT_FOLD_K,
        }
    }
}

impl CellConfig {
    /// Fold a value `mod k` (or pass it through when `k == 0`).
    fn fold(&self, v: u64) -> u64 {
        if self.fold_k == 0 { v } else { v % self.fold_k }
    }
}

/// CellFn v1: keys a point-in-time feature slice into a bounded cell. Holds only
/// its [`CellConfig`] — the key is a pure function of the slice.
#[derive(Clone, Debug, Default)]
pub struct CellFnV1 {
    config: CellConfig,
}

impl CellFnV1 {
    /// A cell function with the default (spec) knobs.
    pub fn new() -> Self {
        Self::default()
    }

    /// A cell function with explicit knobs.
    pub fn with_config(config: CellConfig) -> Self {
        Self { config }
    }

    /// The knobs in force.
    pub fn config(&self) -> &CellConfig {
        &self.config
    }

    /// The channel-value tuple this slice reduces to, in fixed channel order —
    /// the pre-encoding form. `None` marks a channel with nothing observed yet
    /// (distinct from any folded value). Exposed so the injectivity of the
    /// encoding can be tested directly (gate 4).
    pub fn fields(&self, feats: &FeatureSet) -> Vec<Option<u64>> {
        let cfg = &self.config;
        let mut fields = Vec::new();

        if cfg.species_progress {
            let count = feats
                .iter()
                .filter(|f| f.channel == cfg.template_channel)
                .count() as u64;
            fields.push(Some(cfg.species_quant.apply(count)));
        }

        if cfg.last_new_species {
            let last = feats
                .iter()
                .filter(|f| f.channel == cfg.template_channel)
                .map(|f| f.id.0)
                .max();
            fields.push(last.map(|id| cfg.fold(id)));
        }

        for &ch in &cfg.cell_channels {
            let latest = feats
                .iter()
                .filter(|f| f.channel == ch)
                .map(|f| f.id.0)
                .max();
            fields.push(latest.map(|id| cfg.fold(id)));
        }

        fields
    }
}

impl CellFn for CellFnV1 {
    /// Key the slice: reduce to the channel-value tuple, then length-prefix
    /// encode it. Moment-blind — `at` selects which features are *in* the slice,
    /// not the key.
    fn key(&self, _at: Moment, feats: &FeatureSet) -> CellKey {
        encode_cell_key(&self.fields(feats))
    }
}

/// Length-prefixed, **injective** encoding of a channel-value tuple: a `u32`
/// field count, then per field a 1-byte length (`0` = absent, `8` = present)
/// followed by that many little-endian value bytes. Self-delimiting, so distinct
/// tuples always encode to distinct bytes and re-encoding is byte-stable.
pub fn encode_cell_key(fields: &[Option<u64>]) -> CellKey {
    let mut out = Vec::with_capacity(4 + fields.len() * 9);
    out.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    for field in fields {
        match field {
            None => out.push(0),
            Some(v) => {
                out.push(8);
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    out
}

/// The inverse of [`encode_cell_key`]; `None` on malformed bytes (never panics).
/// Present so injectivity is provable by round-trip in tests.
pub fn decode_cell_key(bytes: &[u8]) -> Option<Vec<Option<u64>>> {
    let count = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
    let mut fields = Vec::with_capacity(count);
    let mut i = 4;
    for _ in 0..count {
        match *bytes.get(i)? {
            0 => {
                fields.push(None);
                i += 1;
            }
            8 => {
                let v = u64::from_le_bytes(bytes.get(i + 1..i + 9)?.try_into().ok()?);
                fields.push(Some(v));
                i += 9;
            }
            _ => return None,
        }
    }
    if i == bytes.len() { Some(fields) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorer::{Feature, FeatureId};

    fn feat(channel: u16, id: u64) -> Feature {
        Feature {
            channel: ChannelId(channel),
            id: FeatureId(id),
        }
    }

    #[test]
    fn log2_bucket_groups_by_magnitude() {
        assert_eq!(log2_bucket(0), 0);
        assert_eq!(log2_bucket(1), 1);
        assert_eq!(log2_bucket(2), 2);
        assert_eq!(log2_bucket(3), 2);
        assert_eq!(log2_bucket(4), 3);
        assert_eq!(log2_bucket(7), 3);
        assert_eq!(log2_bucket(8), 4);
    }

    #[test]
    fn empty_slice_keys_the_absent_tuple() {
        let cell = CellFnV1::new();
        let fields = cell.fields(&FeatureSet::new());
        // species-progress present (count 0 → bucket 0), last-new-species absent.
        assert_eq!(fields, vec![Some(0), None]);
    }

    #[test]
    fn fields_read_species_count_and_max_id() {
        let cell = CellFnV1::new();
        let feats: FeatureSet = [feat(1, 5), feat(1, 2), feat(1, 9)].into_iter().collect();
        let fields = cell.fields(&feats);
        // 3 species → log2_bucket(3) = 2; max id 9 mod 64 = 9.
        assert_eq!(fields, vec![Some(2), Some(9)]);
    }

    #[test]
    fn features_on_other_channels_do_not_count_as_species() {
        let cell = CellFnV1::new();
        // channel 7 is not the template channel and not configured → ignored.
        let feats: FeatureSet = [feat(1, 1), feat(7, 4)].into_iter().collect();
        assert_eq!(cell.fields(&feats), vec![Some(1), Some(1)]);
    }

    #[test]
    fn state_channels_fold_their_latest_value() {
        let config = CellConfig {
            cell_channels: vec![ChannelId(2)],
            ..CellConfig::default()
        };
        let cell = CellFnV1::with_config(config);
        // One template species + a state channel whose (single, latest) value is
        // 130 → folded 130 mod 64 = 2.
        let feats: FeatureSet = [feat(1, 0), feat(2, 130)].into_iter().collect();
        assert_eq!(cell.fields(&feats), vec![Some(1), Some(0), Some(2)]);
    }

    #[test]
    fn key_equals_encoded_fields_and_is_moment_blind() {
        let cell = CellFnV1::new();
        let feats: FeatureSet = [feat(1, 3), feat(1, 4)].into_iter().collect();
        let k1 = cell.key(Moment(10), &feats);
        assert_eq!(k1, encode_cell_key(&cell.fields(&feats)));
        assert_eq!(k1, cell.key(Moment(999), &feats), "moment-blind");
    }

    #[test]
    fn encode_decode_roundtrips() {
        let cases: Vec<Vec<Option<u64>>> = vec![
            vec![],
            vec![None],
            vec![Some(0)],
            vec![Some(0), None, Some(64)],
            vec![Some(u64::MAX), Some(0), None, None],
        ];
        for t in cases {
            assert_eq!(decode_cell_key(&encode_cell_key(&t)), Some(t));
        }
    }

    #[test]
    fn distinct_tuples_encode_distinctly() {
        let a = encode_cell_key(&[Some(1), None]);
        let b = encode_cell_key(&[Some(1), Some(0)]);
        let c = encode_cell_key(&[Some(2), None]);
        assert_ne!(a, b, "None vs Some(0) differ");
        assert_ne!(a, c);
        assert_ne!(b, c);
    }

    #[test]
    fn decode_rejects_malformed_bytes() {
        assert_eq!(decode_cell_key(b""), None);
        assert_eq!(decode_cell_key(&[9, 0, 0, 0]), None, "count exceeds data");
        // count=1 then a bad length tag (5).
        assert_eq!(decode_cell_key(&[1, 0, 0, 0, 5]), None);
        // trailing garbage after a complete tuple.
        let mut good = encode_cell_key(&[Some(1)]);
        good.push(0xFF);
        assert_eq!(decode_cell_key(&good), None);
    }

    #[test]
    fn disabling_channels_shrinks_the_tuple() {
        let config = CellConfig {
            species_progress: false,
            last_new_species: false,
            ..CellConfig::default()
        };
        let cell = CellFnV1::with_config(config);
        assert!(cell.fields(&FeatureSet::new()).is_empty());
    }

    #[test]
    fn fold_k_zero_does_not_panic() {
        let config = CellConfig {
            fold_k: 0,
            ..CellConfig::default()
        };
        let cell = CellFnV1::with_config(config);
        let feats: FeatureSet = [feat(1, 1000)].into_iter().collect();
        // last-new-species passes through unfolded when k == 0.
        assert_eq!(cell.fields(&feats), vec![Some(1), Some(1000)]);
    }
}
