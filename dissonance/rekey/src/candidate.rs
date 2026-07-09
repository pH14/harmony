// SPDX-License-Identifier: AGPL-3.0-or-later
//! The candidate space (`docs/SCORING.md` R2) — **declarative configs, not
//! code**.
//!
//! Every candidate is a [`logtmpl::CellConfig`] (the shipped v1 knobs:
//! per-channel enable, quantization, fold modulus) plus, optionally, one
//! **chosen sparse state channel** projected out of the recorded console. That
//! optional channel is the IJON discipline made concrete: *"sparse, chosen state
//! annotations beat indiscriminate state feedback"* — the empty `cell_channels`
//! default is a ruling, not an accident, so a campaign wires in the few state
//! channels it means.
//!
//! ## The chosen state channel, and its twin control
//!
//! The bug-3 workload prints one line per branch carrying the entropy draw it
//! made:
//!
//! ```text
//! UUID_DRAW: draw=0x23e6b1f7c713b0c5 prefix_bits=8
//! ```
//!
//! That draw is the only interesting state the guest exposes, so it is the state
//! channel a campaign author would choose. But bug 3 fires exactly when the
//! draw's **top byte** is `0xA5` — so keying on `draw >> 56`
//! ([`StateProjection::DrawTopByte`]) is *maximally aligned with the trigger*,
//! and a descriptor that looks good only because it was chosen with the answer
//! in hand has learned nothing.
//!
//! [`StateProjection::DrawLowByte`] (`draw & 0xFF`) is its **twin control**:
//! statistically identical (both are one uniform byte of the same draw, 256
//! values, the same arrival pattern, the same `|K|`) and completely
//! trigger-blind. On the unsteered ablation slice the two candidates score
//! identically on breadth *and* granularity, so only a bug-based metric could
//! separate them — which is Böhme–Szekeres–Metzman (ICSE 2022, law 6)
//! demonstrated on harmony's own corpus. It replaces the bug-1 noise-fitting
//! control the task spec assumed, which turned out to be unbuildable (bug 1
//! retained no traces; bead `hm-5sv`, tasks/97 amendment).

use serde::Serialize;

use explorer::{CellFn, CellKey, ChannelId, Feature, FeatureSet};
use logtmpl::{CellConfig, CellFnV1, DEFAULT_FOLD_K, Quant};

use crate::observe::BranchObs;

/// The channel the chosen sparse state feature is filed under. `0` is the
/// explorer's coverage channel and `1` is [`logtmpl::TEMPLATE_CHANNEL`], so the
/// scrape tier's state annotations start at `2`.
pub const STATE_CHANNEL: ChannelId = ChannelId(2);

/// Which sparse projection of the recorded entropy draw a candidate keys on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum StateProjection {
    /// `draw >> 56` — the byte bug 3's 8-bit prefix trigger compares. Aligned
    /// with the trigger by construction.
    DrawTopByte,
    /// `draw & 0xFF` — the same draw's low byte. Statistically identical to
    /// [`StateProjection::DrawTopByte`], and blind to the trigger. The noise
    /// control.
    DrawLowByte,
}

impl StateProjection {
    /// Project a recorded draw onto this channel's feature id.
    pub fn project(self, draw: u64) -> u64 {
        match self {
            StateProjection::DrawTopByte => draw >> 56,
            StateProjection::DrawLowByte => draw & 0xFF,
        }
    }

    /// The projection's name, as the report prints it.
    pub fn label(self) -> &'static str {
        match self {
            StateProjection::DrawTopByte => "draw >> 56 (trigger-aligned)",
            StateProjection::DrawLowByte => "draw & 0xFF (trigger-blind control)",
        }
    }
}

/// One candidate `CellFn` configuration, recorded verbatim in the report.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// A short stable id (the report's row key).
    pub id: &'static str,
    /// One line on what this candidate changes and why it is in the space.
    pub summary: &'static str,
    /// The shipped v1 knobs.
    pub cell: CellConfig,
    /// The chosen sparse state channel, if any.
    pub state: Option<StateProjection>,
}

impl Candidate {
    /// The `CellConfig` as JSON — the "recorded verbatim" form.
    pub fn config_json(&self) -> String {
        // Statically infallible: `CellConfig` is plain integers, bools, and enums.
        serde_json::to_string(&self.cell).expect("CellConfig always serializes")
    }

    /// The candidate's per-branch key stream: re-key one branch's recorded
    /// arrivals under this config.
    ///
    /// One arrival per (marker-filtered) console record, exactly as the campaign
    /// folded it: insert the record's template species into the accumulating
    /// point-in-time slice, insert the chosen state feature if this record
    /// carries one, then key the slice. With no state channel this reproduces
    /// `conductor::benchcampaign::cells_of` byte-for-byte — the harness's own
    /// correctness gate.
    pub fn key_stream(&self, obs: &BranchObs) -> Vec<CellKey> {
        let cells = CellFnV1::with_config(self.cell.clone());
        let mut acc = FeatureSet::new();
        let mut keys = Vec::with_capacity(obs.arrivals.len());
        for arrival in &obs.arrivals {
            acc.insert(Feature {
                channel: logtmpl::TEMPLATE_CHANNEL,
                id: arrival.species,
            });
            if let (Some(projection), Some(draw)) = (self.state, arrival.draw) {
                acc.insert(Feature {
                    channel: STATE_CHANNEL,
                    id: explorer::FeatureId(projection.project(draw)),
                });
            }
            keys.push(cells.key(arrival.at, &acc));
        }
        keys
    }

    /// The candidate's **key-space cardinality** `|K|` — the QD grid size that
    /// normalizes axis (a), computed analytically from the config and the
    /// corpus's observed maxima (raw discovered-cell counts scale with
    /// resolution, so an unnormalized breadth would crown the finest candidate
    /// by construction).
    ///
    /// Field by field, in the order [`CellFnV1`] composes them:
    ///
    /// - *species-progress* — the slice always holds ≥ 1 species when it is
    ///   keyed, so the count ranges over `1..=max_species`: `Log2` collapses that
    ///   to `floor(log2(max_species)) + 1` buckets, `Identity` keeps all
    ///   `max_species`;
    /// - *last-new-species* — the largest id present, `0..max_species`, folded
    ///   `mod k`: `min(max_species, k)` values (`k == 0` is "no fold");
    /// - *the state channel* — `alphabet` values folded `mod k`, **plus one**
    ///   for the absent (`None`) field, which is distinct from every folded
    ///   value and occurs on every arrival before the draw line.
    ///
    /// A candidate with no enabled field keys everything to one cell: `|K| = 1`.
    pub fn key_space(&self, max_species: u64, alphabet: u64) -> u64 {
        let fold = |n: u64| {
            if self.cell.fold_k == 0 {
                n
            } else {
                n.min(self.cell.fold_k)
            }
        };
        let mut k = 1u64;
        if self.cell.species_progress {
            k *= match self.cell.species_quant {
                Quant::Log2 => logtmpl::log2_bucket(max_species.max(1)),
                Quant::Identity => max_species.max(1),
            };
        }
        if self.cell.last_new_species {
            k *= fold(max_species.max(1)).max(1);
        }
        if self.state.is_some() {
            k *= fold(alphabet.max(1)).max(1) + 1;
        }
        k
    }
}

/// Fold a [`CellKey`] to the opaque `u64` the recorded `CampaignLog` carries —
/// FNV-1a over the key bytes.
///
/// A **verbatim mirror** of the private `conductor::benchcampaign::cell_id_of`.
/// Duplicated rather than imported because `conductor` is outside this task's
/// surface and pulls the whole live plane; the harness-correctness gate compares
/// this function's output against the committed campaign logs on all 60
/// campaigns, so a drift between the two would fail loudly rather than silently.
pub fn cell_id_of(key: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in key {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A `CellConfig` differing from the shipped default only in the named knobs.
fn cfg(species_progress: bool, last_new_species: bool, quant: Quant, fold_k: u64) -> CellConfig {
    CellConfig {
        species_progress,
        last_new_species,
        species_quant: quant,
        fold_k,
        ..CellConfig::default()
    }
}

/// A `CellConfig` that also composes the chosen state channel.
fn cfg_state(
    species_progress: bool,
    last_new_species: bool,
    quant: Quant,
    fold_k: u64,
) -> CellConfig {
    CellConfig {
        cell_channels: vec![STATE_CHANNEL],
        ..cfg(species_progress, last_new_species, quant, fold_k)
    }
}

/// The candidate space, in report order. The first entry is **v1 exactly as
/// shipped** — the control every axis must reproduce (spec gate 2).
pub fn candidates() -> Vec<Candidate> {
    use Quant::{Identity, Log2};
    use StateProjection::{DrawLowByte, DrawTopByte};

    vec![
        Candidate {
            id: "v1-shipped",
            summary: "CellFn v1 exactly as the campaign ran it — the control",
            cell: cfg(true, true, Log2, DEFAULT_FOLD_K),
            state: None,
        },
        // The R2 fold_k sweep around DEFAULT_FOLD_K = 64.
        Candidate {
            id: "foldk-16",
            summary: "fold_k = 16",
            cell: cfg(true, true, Log2, 16),
            state: None,
        },
        Candidate {
            id: "foldk-32",
            summary: "fold_k = 32",
            cell: cfg(true, true, Log2, 32),
            state: None,
        },
        Candidate {
            id: "foldk-128",
            summary: "fold_k = 128",
            cell: cfg(true, true, Log2, 128),
            state: None,
        },
        Candidate {
            id: "foldk-256",
            summary: "fold_k = 256",
            cell: cfg(true, true, Log2, 256),
            state: None,
        },
        // The R2 quantization variant.
        Candidate {
            id: "quant-identity",
            summary: "species-progress quantized Identity (raw count) instead of Log2",
            cell: cfg(true, true, Identity, DEFAULT_FOLD_K),
            state: None,
        },
        // Channel-set ablations of v1's composition.
        Candidate {
            id: "species-only",
            summary: "channel ablation: species-progress only (last-new-species off)",
            cell: cfg(true, false, Log2, DEFAULT_FOLD_K),
            state: None,
        },
        Candidate {
            id: "lastnew-only",
            summary: "channel ablation: last-new-species only (species-progress off)",
            cell: cfg(false, true, Log2, DEFAULT_FOLD_K),
            state: None,
        },
        Candidate {
            id: "no-channels",
            summary: "channel ablation: both template channels off — the one-cell floor",
            cell: cfg(false, false, Log2, DEFAULT_FOLD_K),
            state: None,
        },
        // The IJON chosen sparse state channel, and its twin control.
        Candidate {
            id: "draw-top-64",
            summary: "v1 + chosen state channel on the entropy draw's top byte, folded mod 64",
            cell: cfg_state(true, true, Log2, 64),
            state: Some(DrawTopByte),
        },
        Candidate {
            id: "draw-top-256",
            summary: "v1 + chosen state channel on the entropy draw's top byte, unfolded (k = 256)",
            cell: cfg_state(true, true, Log2, 256),
            state: Some(DrawTopByte),
        },
        Candidate {
            id: "draw-top-only-256",
            summary: "the chosen state channel alone (both template channels off), k = 256",
            cell: cfg_state(false, false, Log2, 256),
            state: Some(DrawTopByte),
        },
        Candidate {
            id: "draw-low-256",
            summary: "TWIN CONTROL: identical to draw-top-256 but keyed on the trigger-blind \
                      low byte",
            cell: cfg_state(true, true, Log2, 256),
            state: Some(DrawLowByte),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorer::{FeatureId, Moment};

    use crate::observe::Arrival;

    /// FNV-1a, pinned against the recorded campaign's fold. The empty key hashes
    /// to the offset basis; a one-byte key to the pinned product.
    #[test]
    fn cell_id_mirrors_the_campaigns_fnv1a_fold() {
        assert_eq!(cell_id_of(&[]), 0xcbf2_9ce4_8422_2325);
        assert_eq!(
            cell_id_of(&[0]),
            0xcbf2_9ce4_8422_2325u64.wrapping_mul(0x0000_0100_0000_01b3)
        );
        assert_ne!(cell_id_of(b"a"), cell_id_of(b"b"));
    }

    fn arrival(species: u64, draw: Option<u64>) -> Arrival {
        Arrival {
            at: Moment(0),
            species: FeatureId(species),
            draw,
        }
    }

    /// The control's key stream is exactly v1's: one key per arrival, over the
    /// accumulating species slice, with no state feature inserted.
    #[test]
    fn the_control_keys_only_the_template_channels() {
        let all = candidates();
        let v1 = &all[0];
        assert_eq!(v1.id, "v1-shipped");
        assert!(v1.state.is_none());
        assert!(v1.cell.cell_channels.is_empty(), "the empty default stands");

        let obs = BranchObs {
            branch: 0,
            arrivals: vec![arrival(0, None), arrival(1, Some(0xA5 << 56))],
        };
        let keys = v1.key_stream(&obs);
        assert_eq!(keys.len(), 2, "one key per arrival");
        assert_ne!(keys[0], keys[1], "the second species advances the cell");

        // The draw is present but the control ignores it entirely: a different
        // draw keys identically.
        let other = BranchObs {
            branch: 0,
            arrivals: vec![arrival(0, None), arrival(1, Some(0x11 << 56))],
        };
        assert_eq!(keys, v1.key_stream(&other));
    }

    /// A state-channel candidate keys the chosen projection — and the twin
    /// controls disagree exactly when the two bytes of the draw disagree.
    #[test]
    fn state_channel_candidates_key_their_projection() {
        let all = candidates();
        let top = all.iter().find(|c| c.id == "draw-top-256").expect("top");
        let low = all.iter().find(|c| c.id == "draw-low-256").expect("low");

        // 0xA5..00: the top byte is the trigger, the low byte is zero.
        let firing = BranchObs {
            branch: 0,
            arrivals: vec![arrival(0, Some(0xA5 << 56))],
        };
        // 0x00..A5: the same two bytes, swapped.
        let mirrored = BranchObs {
            branch: 0,
            arrivals: vec![arrival(0, Some(0xA5))],
        };
        assert_ne!(
            top.key_stream(&firing),
            top.key_stream(&mirrored),
            "the top-byte projection separates them"
        );
        assert_eq!(
            top.key_stream(&firing),
            low.key_stream(&mirrored),
            "the two projections are mirror images of one another"
        );
    }

    /// An arrival with no draw leaves the state field absent — a value distinct
    /// from every folded draw (`|K|` counts it).
    #[test]
    fn an_absent_draw_is_its_own_state_value() {
        let all = candidates();
        let top = all.iter().find(|c| c.id == "draw-top-256").expect("top");
        let absent = BranchObs {
            branch: 0,
            arrivals: vec![arrival(0, None)],
        };
        let zero = BranchObs {
            branch: 0,
            arrivals: vec![arrival(0, Some(0))],
        };
        assert_ne!(
            top.key_stream(&absent),
            top.key_stream(&zero),
            "None is not Some(0)"
        );
    }

    /// `|K|` follows the documented product, including the fold and the absent
    /// state value.
    #[test]
    fn key_space_follows_the_documented_product() {
        let all = candidates();
        let get = |id: &str| all.iter().find(|c| c.id == id).expect("candidate").clone();

        // max_species = 4 ⇒ Log2 buckets over 1..=4 are {1,2,3} = log2_bucket(4) = 3.
        // last-new-species over ids 0..3 folded mod 64 ⇒ 4.
        assert_eq!(get("v1-shipped").key_space(4, 256), 3 * 4);
        // Identity keeps the raw count 1..=4 ⇒ 4 × 4.
        assert_eq!(get("quant-identity").key_space(4, 256), 4 * 4);
        // Ablations drop a factor each; both off is the one-cell floor.
        assert_eq!(get("species-only").key_space(4, 256), 3);
        assert_eq!(get("lastnew-only").key_space(4, 256), 4);
        assert_eq!(get("no-channels").key_space(4, 256), 1);
        // fold_k = 16 cannot fold ids that are already below it.
        assert_eq!(get("foldk-16").key_space(4, 256), 3 * 4);
        // The state channel multiplies by min(alphabet, k) + 1 (the absent value).
        assert_eq!(get("draw-top-256").key_space(4, 256), 3 * 4 * 257);
        assert_eq!(get("draw-top-64").key_space(4, 256), 3 * 4 * 65);
        assert_eq!(get("draw-top-only-256").key_space(4, 256), 257);
        assert_eq!(
            get("draw-low-256").key_space(4, 256),
            get("draw-top-256").key_space(4, 256),
            "the twin control has exactly the same key space"
        );
    }

    /// Every candidate id is unique and every config serializes verbatim.
    #[test]
    fn the_candidate_space_is_well_formed() {
        let all = candidates();
        let ids: std::collections::BTreeSet<&str> = all.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), all.len(), "ids are unique");
        for c in &all {
            let json = c.config_json();
            assert!(json.contains("fold_k"), "{}: {json}", c.id);
            assert_eq!(
                c.cell.cell_channels.is_empty(),
                c.state.is_none(),
                "{}: a state projection iff a state channel",
                c.id
            );
        }
    }
}
