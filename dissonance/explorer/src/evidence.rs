// SPDX-License-Identifier: AGPL-3.0-or-later
//! The completed-run **evidence view** and the deterministic observation
//! reduction + cell projection at a seal's evidence cut (`hm-bbx.4`).
//!
//! This is the semantic core of the Differential observation plane, expressed as
//! **pure functions of persisted evidence** (`docs/DISSONANCE-STRATEGY.md`:
//! "Differential derivations are pure functions of persisted evidence and one
//! immutable campaign configuration"). Nothing here executes or schedules a VM.
//!
//! ## The immutable completed-run view
//!
//! [`CompletedRunEvidence`] is the borrowed, immutable view an
//! [`Oracle`](crate::occurrence) reads **after** the run's evidence is durably
//! appended: terminal identity, reproducer identity, the normalized
//! schema/events, and their coordinates. It is not a second mutable-ledger
//! interface and not a duplicate event authority — it is one owned artifact,
//! borrowed read-only.
//!
//! ## Reduction at a cut — half-open by prefix length, never by `Moment`
//!
//! [`reduce_at_cut`] computes the complete point-in-time observation map at a
//! seal's [`EvidenceCut`]. The cut is half-open **by the included SDK-event
//! count** (the `hm-bbx.6` prefix length): an event participates iff its
//! `ordinal < included` — never a `Moment` comparison, because several events
//! may share one stamped `Moment` and only the prefix length cuts them exactly.
//! Each reducible-state observation is reduced **independently** under its
//! declared base operation (`set`/`max`/`min`/`accumulate`); an occurrence, an
//! unresolved-legacy state point, or numeric guidance is not reduced into the
//! cell (it stays timestamped evidence). This is the strategy's "each independent
//! observation is reduced independently before the complete point-in-time state
//! is passed to `CellFn`" — no packed `(register, value)` feature.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::spine::{CellKey, EvidenceCut, Moment};
use crate::{Reproducer, StopReason};
use sdk_events::{Normalized, ObservationId, Payload, SdkEvent, SdkSchema, UpdateOp};

/// A deterministic rollout identity: the campaign/config-scoped stream position a
/// rollout was issued at, plus whether it is a genesis-rooted run or a branch
/// child (branch ingestion appends only positions **after** the parent cut under
/// the child rollout identity — the restored ancestor prefix is inherited through
/// lineage, never duplicated as child evidence).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct RunId {
    /// The campaign-seeded issue index of the rollout (deterministic order).
    pub issue: u64,
    /// The parent rollout this run branched from, if any (`None` for a
    /// genesis-rooted run). Lineage, so an ancestor prefix is inherited and never
    /// re-appended as child evidence.
    pub parent: Option<u64>,
}

/// The reduced value of one state observation at a cut. `Scalar` covers `set`,
/// `max`, and `min` (all collapse to one integer); `Accumulated` covers the set
/// of distinct values an `accumulate` register has seen. No floating point ever
/// (conventions rule 4).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum ReducedValue {
    /// A single reduced integer (the current `set` value, or the running
    /// `max`/`min`).
    Scalar(u64),
    /// The set of distinct values an `accumulate` register observed, canonically
    /// ordered.
    Accumulated(BTreeSet<u64>),
}

/// The complete point-in-time observation map at a seal: every reducible-state
/// observation independently reduced under its declared base operation, keyed by
/// its stable identity. Deterministically ordered (`BTreeMap`), so no iteration
/// order can reach a cell key.
pub type ObservationMap = BTreeMap<ObservationId, ReducedValue>;

/// Reduce the normalized events of a completed run to the observation map true at
/// the cut `included` (the seal's included SDK-event count).
///
/// **Half-open by prefix length, by SDK-event vector position**: the first
/// `included` events of the ordered [`Normalized`](sdk_events::Normalized) event
/// vector participate — never a `Moment` comparison (`hm-bbx.6`: several events
/// may share one stamped `Moment`), and never the catalog-gapped
/// [`ordinal`](sdk_events::SdkEvent::ordinal) (the schema declaration is not an
/// event and occupies no cut position). The events are in persisted order, so the
/// first `included` are exactly the seal's prefix, including the subset emitted
/// *at* the seal's `Moment`. Only observations the `schema` declares
/// [reducible](sdk_events::SchemaEntry::is_reducible_state) — resolved `u64`
/// state with a declared base op — participate; occurrences, unresolved legacy
/// state, and numeric guidance are left out (they remain timestamped evidence a
/// derivation or oracle may consult, never silently reduced into a cell). Each
/// observation is reduced **independently** under its schema base op, so distinct
/// registers keep distinct dimensions.
pub fn reduce_at_cut(events: &[SdkEvent], schema: &SdkSchema, included: u64) -> ObservationMap {
    let mut out: ObservationMap = BTreeMap::new();
    for ev in events.iter().take(included as usize) {
        let Payload::State { value, .. } = &ev.payload else {
            continue;
        };
        // The schema's declared base op is the authority (a v1 firing never
        // blesses a reducer); an unresolved or non-`u64` state point is not
        // reducible and stays evidence only.
        let Some(entry) = schema.entry(&ev.id) else {
            continue;
        };
        if !entry.is_reducible_state() {
            continue;
        }
        let op = entry.base_op.expect("is_reducible_state ⇒ base_op is Some");
        let v = *value;
        match op {
            // `set`: the latest value at or before the cut — events are in ordinal
            // order and this is the prefix, so the last write wins.
            UpdateOp::Set => {
                out.insert(ev.id.clone(), ReducedValue::Scalar(v));
            }
            UpdateOp::Max => {
                let slot = out.entry(ev.id.clone()).or_insert(ReducedValue::Scalar(v));
                if let ReducedValue::Scalar(cur) = slot {
                    *cur = (*cur).max(v);
                }
            }
            UpdateOp::Min => {
                let slot = out.entry(ev.id.clone()).or_insert(ReducedValue::Scalar(v));
                if let ReducedValue::Scalar(cur) = slot {
                    *cur = (*cur).min(v);
                }
            }
            UpdateOp::Accumulate => {
                let slot = out
                    .entry(ev.id.clone())
                    .or_insert_with(|| ReducedValue::Accumulated(BTreeSet::new()));
                if let ReducedValue::Accumulated(set) = slot {
                    set.insert(v);
                }
            }
        }
    }
    out
}

/// The **cell projection** at a seal's `sealed_at`: a pure, versioned function
/// from the complete materialized observation map to one opaque [`CellKey`]. This
/// is the `CellFn` role the strategy keeps — evaluated on the complete projected
/// observations at the actual `sealed_at`, over independently-reduced
/// observations rather than a packed feature set. (The spine's legacy
/// [`CellFn`](crate::CellFn) over a `FeatureSet` is retained for the log-template
/// consumer; this is the Differential-plane projection.)
pub trait ObservationCells {
    /// Key the observation map true at `cut` into an opaque cell.
    fn key(&self, cut: EvidenceCut, obs: &ObservationMap) -> CellKey;
}

/// The default observation cell projection: the canonical byte encoding of the
/// reduced observation state (each `(identity, reduced value)` in sorted order).
/// **Moment-blind** by default — the same reduced state is the same cell wherever
/// it occurs (progress, not wall position), exactly the finest useful keying the
/// legacy `IdentityCells` gave, but over per-observation reductions.
#[derive(Clone, Debug, Default)]
pub struct DefaultObservationCells;

impl DefaultObservationCells {
    /// The default observation cell projection (stateless).
    pub fn new() -> Self {
        Self
    }
}

impl ObservationCells for DefaultObservationCells {
    fn key(&self, _cut: EvidenceCut, obs: &ObservationMap) -> CellKey {
        let mut key = Vec::new();
        for (id, val) in obs {
            encode_observation_id(&mut key, id);
            encode_reduced_value(&mut key, val);
        }
        key
    }
}

/// Canonically encode an observation identity into a cell key (domain-tagged so
/// the three variants can never alias, length-prefixed so no two strings run
/// together).
pub(crate) fn encode_observation_id(out: &mut Vec<u8>, id: &ObservationId) {
    match id {
        ObservationId::Point { namespace, local } => {
            out.push(0x01);
            out.push(*namespace);
            out.extend_from_slice(&local.to_le_bytes());
        }
        ObservationId::Property(s) => {
            out.push(0x02);
            out.extend_from_slice(&(s.len() as u64).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        ObservationId::Lifecycle(s) => {
            out.push(0x03);
            out.extend_from_slice(&(s.len() as u64).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
    }
}

/// Decode one canonically encoded observation identity (the exact inverse of
/// [`encode_observation_id`]). Total: `None` on any malformed encoding, never
/// a panic — the production caller feeds only its own encoder's output, so a
/// `None` there is an internal-invariant break the caller surfaces loudly.
pub(crate) fn decode_observation_id(bytes: &[u8]) -> Option<ObservationId> {
    let (&tag, rest) = bytes.split_first()?;
    match tag {
        0x01 => {
            let (&namespace, rest) = rest.split_first()?;
            let local = u32::from_le_bytes(rest.try_into().ok()?);
            Some(ObservationId::Point { namespace, local })
        }
        0x02 | 0x03 => {
            if rest.len() < 8 {
                return None;
            }
            let (len, s) = rest.split_at(8);
            // Statically infallible: split_at(8) yields exactly 8 bytes.
            let len = u64::from_le_bytes(len.try_into().expect("8-byte slice"));
            if s.len() as u64 != len {
                return None;
            }
            let s = String::from_utf8(s.to_vec()).ok()?;
            Some(if tag == 0x02 {
                ObservationId::Property(s)
            } else {
                ObservationId::Lifecycle(s)
            })
        }
        _ => None,
    }
}

/// Canonically encode a reduced value into a cell key.
fn encode_reduced_value(out: &mut Vec<u8>, val: &ReducedValue) {
    match val {
        ReducedValue::Scalar(v) => {
            out.push(0x01);
            out.extend_from_slice(&v.to_le_bytes());
        }
        ReducedValue::Accumulated(set) => {
            out.push(0x02);
            out.extend_from_slice(&(set.len() as u64).to_le_bytes());
            for v in set {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
}

/// Which committed record a batch is — a completed **rollout** submitted at one
/// revision, or its later materialized **seal** submitted at another (the
/// strategy's "one search step may submit a completed rollout at one revision
/// and its later materialized seal at another"). Carried explicitly so durable
/// records stay distinguishable without heuristics: the retention views' rebuild
/// admits only seal batches to occupancy and judges only rollout batches.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum EvidenceRole {
    /// A completed open-loop rollout's terminal evidence (full SDK prefix cut).
    Rollout,
    /// A materialized seal's evidence at its actual server-stamped `sealed_at`.
    Seal,
}

/// One completed rollout's **immutable evidence**: the durable, borrow-only view
/// the campaign appends to the evidence ledger and an [`Oracle`](crate::occurrence)
/// judges. Carrying [`Normalized`] (schema + ordered `SdkEvent`s + stream
/// commitment) makes it self-validating on reload — the only publicly
/// deserializable sdk-events artifact re-decodes its own bytes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CompletedRunEvidence {
    /// The deterministic rollout identity (issue order + lineage).
    pub rollout: RunId,
    /// Which committed record this batch is (rollout vs materialized seal).
    pub role: EvidenceRole,
    /// The terminal stop that ended the rollout.
    pub terminal: StopReason,
    /// The genesis-complete reproducer that regenerates the run (identity, not a
    /// second execution authority).
    pub env: Reproducer,
    /// The evidence cut this batch is anchored at — the candidate `observed_at`
    /// for a completed rollout, or the actual `sealed_at` for a materialized
    /// seal. Half-open by `sdk_events` (the included count), never by `at`.
    pub cut: EvidenceCut,
    /// The normalized SDK evidence (schema + ordered events + commitment).
    pub normalized: Normalized,
    /// The branch-point cut on the parent rollout's evidence vector (`None`
    /// for a genesis-rooted run): the lineage authority for prefix
    /// composition. A branch child's `normalized` carries only its own
    /// suffix; positions are cumulative from `parent_cut.count` (task 132).
    #[serde(default)]
    pub parent_cut: Option<EvidenceCut>,
    /// The sealable-point moments this rollout observed, in observation
    /// order — the provisional-cut nomination coordinates, persisted so a
    /// restart re-stages the exact same relation inputs (task 132). Empty
    /// for a seal batch.
    #[serde(default)]
    pub sealable_moments: Vec<u64>,
}

impl CompletedRunEvidence {
    /// The reduced observation map true at this evidence's own cut (the
    /// convenience the occupancy reduction and the cell projection consume).
    pub fn observations_at_cut(&self) -> ObservationMap {
        reduce_at_cut(
            &self.normalized.events,
            &self.normalized.schema,
            self.cut.sdk_events,
        )
    }

    /// The reduced observation map at an arbitrary earlier cut on the same
    /// evidence (a provisional unsealed cut nominates replay from here). Panics
    /// never: an out-of-range `included` simply includes the whole prefix.
    pub fn observations_at(&self, included: u64) -> ObservationMap {
        reduce_at_cut(&self.normalized.events, &self.normalized.schema, included)
    }

    /// Canonical, deterministic bytes of this evidence — the content the durable
    /// batch identity digests and the ledger stores. `serde_json` over
    /// `BTreeMap`/`Vec`-only fields is byte-stable across platforms.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        // Infallible for our owned, finite, non-float types; a serialize error
        // here would be a programming error, not untrusted input.
        serde_json::to_vec(self).expect("CompletedRunEvidence serializes")
    }

    /// The V-time of this evidence's cut (a convenience for ordering candidates).
    pub fn at(&self) -> Moment {
        self.cut.at
    }
}

/// The **lineage-composed** reduced observation map at `included` (a
/// cumulative position count): the ancestor evidence segments through their
/// fork cuts, root-first, plus this batch's own suffix — reduced under this
/// batch's schema. This is the direct-recomputation ORACLE for the
/// production Differential relations ("direct recomputation is an oracle,
/// not a second backend"), and the retention fold's cell authority.
///
/// The chain walks `rollout.parent` through the ledger's **rollout** batches
/// (a seal batch's `parent` is the rollout it seals, whose own `parent_cut`
/// continues the chain). A collected (GC'd) ancestor contributes nothing —
/// composition proceeds over the retained prefix (the retention rules keep
/// live-Entry lineage collectible only behind a covering checkpoint).
pub fn compose_observations_at(
    ledger: &crate::ledger::EvidenceLedger,
    ev: &CompletedRunEvidence,
    included: u64,
) -> ObservationMap {
    // Collect ancestor segments child-first: (segment events, start, upper).
    // Positions are explicit — `start` is each batch's own cumulative base
    // (its `parent_cut` count) — because the composed prefix may begin above
    // zero: a pre-campaign (setup) prefix restored into the genesis base is
    // inherited machine state that belongs to no rollout batch, so cumulative
    // position and vector index differ by the root's start.
    let mut segments: Vec<(Vec<SdkEvent>, u64, u64)> = Vec::new();
    let mut parent = ev.rollout.parent;
    let mut upper = ev.parent_cut.map(|c| c.sdk_events).unwrap_or(0);
    while let Some(issue) = parent {
        let Some(anc) = ledger
            .batch_ids()
            .filter_map(|id| ledger.get(id))
            .find(|b| b.role == EvidenceRole::Rollout && b.rollout.issue == issue)
        else {
            break; // collected or foreign ancestor: compose the retained prefix
        };
        let start = anc.parent_cut.map(|c| c.sdk_events).unwrap_or(0);
        segments.push((anc.normalized.events.clone(), start, upper));
        parent = anc.rollout.parent;
        upper = start;
    }
    // Assemble the position-filtered prefix root-first: each ancestor's own
    // events at cumulative positions `start + i`, truncated at its child's
    // fork count, then this batch's own suffix — keeping exactly the
    // positions `< included` (the half-open cut is by cumulative position).
    let mut events: Vec<SdkEvent> = Vec::new();
    for (seg, start, upper) in segments.into_iter().rev() {
        for (i, e) in seg.into_iter().enumerate() {
            let pos = start + i as u64;
            if pos < upper && pos < included {
                events.push(e);
            }
        }
    }
    let own_start = ev.parent_cut.map(|c| c.sdk_events).unwrap_or(0);
    for (i, e) in ev.normalized.events.iter().enumerate() {
        let pos = own_start + i as u64;
        if pos < included {
            events.push(e.clone());
        }
    }
    let filtered = events.len() as u64;
    reduce_at_cut(&events, &ev.normalized.schema, filtered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdk_events::{Classification, NS_STATE, ValueShape};
    use sdk_events::{DeclaredPoint, decode_binary};

    /// A v2 catalog declaring one reducible `u64` state register under `op`.
    fn v2_state_decl(local: u32, op: UpdateOp) -> Vec<u8> {
        sdk_events::encode_v2_declaration(&[DeclaredPoint {
            namespace: NS_STATE,
            local,
            name: "reg".into(),
            classification: Classification::State,
            value_shape: Some(ValueShape::U64),
            base_op: Some(op),
            expectation: None,
        }])
        .expect("valid v2 declaration")
    }

    /// The wire op byte for a state firing (aligned with `UpdateOp`'s bytes:
    /// Set=0, Max=1, Min=2, Accumulate=3) — the decoder rejects a firing whose
    /// op disagrees with the declared base op (`MixedOperations`).
    fn op_byte(op: UpdateOp) -> u8 {
        match op {
            UpdateOp::Set => 0,
            UpdateOp::Max => 1,
            UpdateOp::Min => 2,
            UpdateOp::Accumulate => 3,
        }
    }

    fn state_firing(op: UpdateOp, value: u64) -> Vec<u8> {
        let mut b = vec![op_byte(op)];
        b.extend_from_slice(&value.to_le_bytes());
        b
    }

    fn event_id(local: u32) -> u32 {
        ((NS_STATE as u32) << 24) | (local & 0x00FF_FFFF)
    }

    /// Decode a v2 catalog + a sequence of state firings into `Normalized`.
    fn normalized(op: UpdateOp, firings: &[u64]) -> Normalized {
        let mut raw: Vec<(sdk_events::Moment, u32, Vec<u8>)> =
            vec![(sdk_events::Moment(0), 0, v2_state_decl(7, op))];
        for (i, &v) in firings.iter().enumerate() {
            raw.push((
                sdk_events::Moment(10 + i as u64),
                event_id(7),
                state_firing(op, v),
            ));
        }
        decode_binary(&raw).expect("decodes")
    }

    fn reg7() -> ObservationId {
        ObservationId::Point {
            namespace: NS_STATE,
            local: 7,
        }
    }

    /// `set` keeps the latest value in the prefix; the cut is half-open by the
    /// SDK-event vector prefix length (a count of included events).
    #[test]
    fn set_reduces_to_latest_within_the_half_open_prefix() {
        let n = normalized(UpdateOp::Set, &[3, 9, 5]);
        assert_eq!(n.events.len(), 3, "three firings (the catalog is schema)");
        // Whole prefix (all three included): latest write (5) wins.
        let obs = reduce_at_cut(&n.events, &n.schema, 3);
        assert_eq!(obs.get(&reg7()), Some(&ReducedValue::Scalar(5)));
        // Cut of length 2: latest of the first two writes (9).
        let obs = reduce_at_cut(&n.events, &n.schema, 2);
        assert_eq!(obs.get(&reg7()), Some(&ReducedValue::Scalar(9)));
        // Cut of length 1: only the first write (3).
        let obs = reduce_at_cut(&n.events, &n.schema, 1);
        assert_eq!(obs.get(&reg7()), Some(&ReducedValue::Scalar(3)));
        // Empty cut: the register is absent (nothing reduced yet).
        let obs = reduce_at_cut(&n.events, &n.schema, 0);
        assert!(obs.is_empty());
    }

    /// `max`/`min` keep the running extremum over the prefix.
    #[test]
    fn max_and_min_reduce_to_the_running_extremum() {
        let n = normalized(UpdateOp::Max, &[3, 9, 5]);
        let obs = reduce_at_cut(&n.events, &n.schema, u64::MAX);
        assert_eq!(obs.get(&reg7()), Some(&ReducedValue::Scalar(9)));
        let n = normalized(UpdateOp::Min, &[3, 9, 5]);
        let obs = reduce_at_cut(&n.events, &n.schema, u64::MAX);
        assert_eq!(obs.get(&reg7()), Some(&ReducedValue::Scalar(3)));
    }

    /// `accumulate` keeps the set of distinct values seen.
    #[test]
    fn accumulate_reduces_to_the_distinct_value_set() {
        let n = normalized(UpdateOp::Accumulate, &[3, 9, 3, 5]);
        let obs = reduce_at_cut(&n.events, &n.schema, u64::MAX);
        let want: BTreeSet<u64> = [3, 9, 5].into_iter().collect();
        assert_eq!(obs.get(&reg7()), Some(&ReducedValue::Accumulated(want)));
    }

    /// Independent registers reduce independently (no packing into one feature).
    #[test]
    fn independent_registers_stay_independent_dimensions() {
        let decl = sdk_events::encode_v2_declaration(&[
            DeclaredPoint {
                namespace: NS_STATE,
                local: 1,
                name: "a".into(),
                classification: Classification::State,
                value_shape: Some(ValueShape::U64),
                base_op: Some(UpdateOp::Set),
                expectation: None,
            },
            DeclaredPoint {
                namespace: NS_STATE,
                local: 2,
                name: "b".into(),
                classification: Classification::State,
                value_shape: Some(ValueShape::U64),
                base_op: Some(UpdateOp::Max),
                expectation: None,
            },
        ])
        .expect("valid");
        let raw = vec![
            (sdk_events::Moment(0), 0, decl),
            (
                sdk_events::Moment(1),
                event_id(1),
                state_firing(UpdateOp::Set, 7),
            ),
            (
                sdk_events::Moment(2),
                event_id(2),
                state_firing(UpdateOp::Max, 4),
            ),
            (
                sdk_events::Moment(3),
                event_id(2),
                state_firing(UpdateOp::Max, 9),
            ),
        ];
        let n = decode_binary(&raw).expect("decodes");
        let obs = reduce_at_cut(&n.events, &n.schema, u64::MAX);
        let a = ObservationId::Point {
            namespace: NS_STATE,
            local: 1,
        };
        let b = ObservationId::Point {
            namespace: NS_STATE,
            local: 2,
        };
        assert_eq!(obs.get(&a), Some(&ReducedValue::Scalar(7)));
        assert_eq!(obs.get(&b), Some(&ReducedValue::Scalar(9)));
        // Two dimensions, keyed separately — not one packed value.
        assert_eq!(obs.len(), 2);
    }

    /// The default cell projection is a pure function of the observation map and
    /// distinguishes different reduced states.
    #[test]
    fn default_cells_key_is_pure_and_discriminating() {
        let cut = EvidenceCut {
            at: Moment(40),
            sdk_events: 3,
        };
        let cells = DefaultObservationCells::new();
        let mut a: ObservationMap = BTreeMap::new();
        a.insert(reg7(), ReducedValue::Scalar(5));
        let mut b: ObservationMap = BTreeMap::new();
        b.insert(reg7(), ReducedValue::Scalar(6));
        assert_eq!(cells.key(cut, &a), cells.key(cut, &a));
        assert_ne!(cells.key(cut, &a), cells.key(cut, &b));
        // Moment-blind: same observations, different cut moment, same key.
        let cut2 = EvidenceCut {
            at: Moment(80),
            sdk_events: 3,
        };
        assert_eq!(cells.key(cut, &a), cells.key(cut2, &a));
        // The empty map keys to the empty cell.
        assert!(cells.key(cut, &BTreeMap::new()).is_empty());
    }
}
