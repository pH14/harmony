// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nova-owned archive keys, state preferences, and report shapes.

use std::{cmp::Ordering, error::Error, num::NonZeroUsize};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    nova::target::{
        ButtonChord, NovaInput, NovaMechanicalState, NovaObservations, NovaSnapshot,
        preference_tuple,
    },
    search::{
        archive::{
            Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, ReplacementPolicy,
            SelectorAccounting, SelectorPolicy, entries_by_suffix,
        },
        rand::RomuDuoJrRand,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;

/// Largest bounded input horizon accepted by a Nova campaign.
pub const MAX_NOVA_ACTIONS: usize = 8_192;
/// Recorded archive-key and per-location preference policy.
/// Recorded archive-key and per-location preference policy for a run
/// retaining one arrival per location.
pub const KEY_POLICY_IDENTIFIER: &str = "nova_spatial_16_ability_preference_v1";

/// Widest work-RAM fingerprint a run may retain slots by.
pub const MAX_FINGERPRINT_BITS: u8 = 8;

/// The recorded key policy for a fingerprint width. Zero bits reproduce
/// [`KEY_POLICY_IDENTIFIER`] exactly, so a run that keeps one arrival per
/// location records what it always did.
#[must_use]
pub fn key_policy_identifier(fingerprint_bits: u8) -> String {
    if fingerprint_bits == 0 {
        KEY_POLICY_IDENTIFIER.to_owned()
    } else {
        format!("nova_spatial_16_ability_preference_fingerprint{fingerprint_bits}_v1")
    }
}
/// Recorded same-slot replacement policy keeping the cheapest route.
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
/// Prefix of the recorded replacement policy that splits a slot once enough
/// arrivals have been rejected against a tried, childless incumbent; the
/// rejection and draw counts follow the colon.
pub const REPLACEMENT_OR_BARREN_PREFIX: &str =
    "opaque_preference_then_fewest_frames_or_pressured_split:";
/// Recorded replacement policy splitting every slot by variant from the start.
pub const REPLACEMENT_SPLIT_BY_VARIANT_IDENTIFIER: &str =
    "opaque_preference_then_fewest_frames_per_variant";

/// The recorded identifier of a replacement policy.
#[must_use]
pub fn replacement_identifier(policy: ReplacementPolicy) -> String {
    match policy {
        ReplacementPolicy::FewestFrames => REPLACEMENT_IDENTIFIER.to_owned(),
        ReplacementPolicy::SplitByVariant => REPLACEMENT_SPLIT_BY_VARIANT_IDENTIFIER.to_owned(),
        ReplacementPolicy::FewestFramesOrPressuredSplit { rejections, draws } => {
            format!("{REPLACEMENT_OR_BARREN_PREFIX}{rejections},{draws}")
        }
    }
}

/// The replacement policy a recorded identifier names.
///
/// # Errors
///
/// Returns an error when the identifier names no compiled policy or carries
/// a zero count.
pub fn replacement_from_identifier(identifier: &str) -> Result<ReplacementPolicy, Box<dyn Error>> {
    if identifier == REPLACEMENT_IDENTIFIER {
        return Ok(ReplacementPolicy::FewestFrames);
    }
    if identifier == REPLACEMENT_SPLIT_BY_VARIANT_IDENTIFIER {
        return Ok(ReplacementPolicy::SplitByVariant);
    }
    if let Some(values) = identifier.strip_prefix(REPLACEMENT_OR_BARREN_PREFIX) {
        let (rejections, draws) = values
            .split_once(',')
            .ok_or("pressured split needs a rejection count and a draw count")?;
        let rejections = rejections.parse::<u64>()?;
        let draws = draws.parse::<u64>()?;
        if rejections == 0 || draws == 0 {
            return Err("pressured split counts must be nonzero".into());
        }
        return Ok(ReplacementPolicy::FewestFramesOrPressuredSplit { rejections, draws });
    }
    Err(format!("Nova replacement policy {identifier} is not recognized").into())
}
/// Recorded controller hold distribution.
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

/// The digest mask retaining `bits` variants of otherwise-hidden state.
///
/// Nova's key names a location and the durable resources; everything else
/// the machine is holding -- pose, momentum, what the level's own actors are
/// doing -- is invisible to it. Two arrivals at one location that differ
/// only in that state then contend for a single slot, and the cheaper one
/// keeps it however badly it is placed. These bits bound the split, and the
/// grouping pools them away above depth 0 so selection still sees one cell.
///
/// The width is a run policy because it is not one trade. Level 9 cannot be
/// cleared without it: its corridor cell took 123 selections and had all 411
/// candidates rejected as duplicates, and four configurations stalled at the
/// same pixel, one at 1.6M executions, while six bits clear it in 154,281.
/// Three bits are worse than none, diluting draws without separating enough
/// to cross. Level 25 needs none of it and pays for the dilution. Pick the
/// width per workload and record it.
fn fingerprint_mask(bits: u8) -> u8 {
    match bits.min(MAX_FINGERPRINT_BITS) {
        0 => 0,
        bits => u8::try_from((1_u16 << bits).saturating_sub(1)).unwrap_or(u8::MAX),
    }
}

/// The parent selector named by a stream, resolved under Nova's group depths.
pub fn selector_policy_from_identifier(identifier: &str) -> Result<SelectorPolicy, Box<dyn Error>> {
    crate::search::archive::selector_policy_from_identifier(
        identifier,
        NovaArchiveKey::groups().saturating_sub(2),
    )
}

/// Nova's archive instantiation.
pub type NovaArchive = Archive<ButtonChord, NovaArchiveKey, NovaMilestones, NovaSnapshot>;

/// Opaque pooled identity returned to the generic selector.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NovaArchiveGroup {
    cleared: u8,
    collectibles: u8,
    available: u8,
    started_level: u8,
    level: u8,
    x: u16,
    y: u16,
    ability: u8,
}

/// Quality-diversity key for one Nova endpoint.
///
/// Field order is load-bearing and measured. The generic archive reads a
/// key's `Ord` as depth, and this one ranks the resource fields ahead of
/// position, so the ordering and the declared grouping disagree: see
/// [`first_depth_ord_disagreement`](crate::search::archive::first_depth_ord_disagreement).
/// Ranking progress first is the tidier contract and it searched worse.
/// Level 25 at seed 1 under the same draw policy cleared in 104,751
/// executions with resources first and had not cleared by 258,900 with
/// position first, because the splice donor gate then stops preferring
/// donors that reached a cell without taking damage. Keep the order until a
/// paired measurement says otherwise.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct NovaArchiveKey {
    /// Durable completed-level count.
    pub cleared: u8,
    /// Durable collectible count.
    pub collectibles: u8,
    /// Unlocked-level count.
    pub available: u8,
    /// Whether an ability is carried.
    pub has_ability: bool,
    /// Which ability is carried. Two states at one location carrying
    /// different abilities are different slots: what an ability lets the
    /// player do decides whether a location can be left, and a level entered
    /// with one carried in cannot be cleared by pretending the state that
    /// picked up another is a duplicate of it.
    pub ability: u8,
    /// Current health.
    pub health: u8,
    /// Current puzzle-chip count.
    pub chips: u8,
    /// Selected campaign level.
    pub started_level: u8,
    /// Internal map number.
    pub level: u8,
    /// Player horizontal 16-pixel bucket.
    pub x: u16,
    /// Player vertical 16-pixel bucket.
    pub y: u16,
    /// Digest bits of work RAM, the key's [`ArchiveKey::variant`]. No group
    /// reads it, it takes no part in the key's identity or ordering, and only
    /// a replacement policy that splits barren slots reads it.
    #[serde(default)]
    pub state_fingerprint: u8,
}

impl NovaArchiveKey {
    /// Every field but the variant, in the measured order. The variant is
    /// carried, recorded, and read by a splitting replacement policy, but it
    /// is not part of the key's identity: two arrivals that differ only in
    /// it are the same key, exactly as they were before it existed, so a
    /// run that never splits a slot searches as it always did.
    fn identity(self) -> (u8, u8, u8, bool, u8, u8, u8, u8, u8, u16, u16) {
        (
            self.cleared,
            self.collectibles,
            self.available,
            self.has_ability,
            self.ability,
            self.health,
            self.chips,
            self.started_level,
            self.level,
            self.x,
            self.y,
        )
    }
}

impl PartialEq for NovaArchiveKey {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl Eq for NovaArchiveKey {}

impl PartialOrd for NovaArchiveKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NovaArchiveKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.identity().cmp(&other.identity())
    }
}

impl ArchiveKey for NovaArchiveKey {
    type Group = NovaArchiveGroup;

    fn groups() -> usize {
        5
    }

    /// Depth 0 is one 16-pixel location, depth 1 one 32-pixel selection cell,
    /// depth 2 a durable-progress 128-pixel region, depth 3 its level, and
    /// depth 4 durable progress alone. Resource fields never multiply slots;
    /// they decide which one representative remains at a location.
    fn group(self, depth: usize) -> Self::Group {
        let location = NovaArchiveGroup {
            started_level: self.started_level,
            level: self.level,
            x: self.x,
            y: self.y,
            ..NovaArchiveGroup::default()
        };
        match depth {
            0 => NovaArchiveGroup {
                ability: self.ability,
                ..location
            },
            1 => NovaArchiveGroup {
                x: self.x / 2,
                y: self.y / 2,
                ..location
            },
            2 => NovaArchiveGroup {
                cleared: self.cleared,
                collectibles: self.collectibles,
                available: self.available,
                x: self.x / 8,
                y: self.y / 8,
                ..location
            },
            3 => NovaArchiveGroup {
                cleared: self.cleared,
                collectibles: self.collectibles,
                available: self.available,
                started_level: self.started_level,
                level: self.level,
                ..NovaArchiveGroup::default()
            },
            _ => NovaArchiveGroup {
                cleared: self.cleared,
                collectibles: self.collectibles,
                available: self.available,
                ..NovaArchiveGroup::default()
            },
        }
    }

    fn slot_capacity() -> usize {
        1
    }

    fn preference_cmp(self, other: Self) -> Ordering {
        self.preference().cmp(&other.preference())
    }

    fn variant(self) -> u8 {
        self.state_fingerprint
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

impl NovaArchiveKey {
    fn preference(self) -> (u8, u8, u8, bool, u8, u8) {
        (
            self.cleared,
            self.collectibles,
            self.available,
            self.has_ability,
            self.health,
            self.chips,
        )
    }
}

/// Build the opaque archive key from a decoded Nova state.
#[must_use]
pub fn archive_key(
    state: NovaMechanicalState,
    work_ram: &[u8],
    fingerprint_bits: u8,
) -> NovaArchiveKey {
    let (cleared, collectibles, available, has_ability, health, chips) = preference_tuple(state);
    let mask = fingerprint_mask(fingerprint_bits);
    let state_fingerprint = if mask == 0 {
        0
    } else {
        Sha256::digest(work_ram)[0] & mask
    };
    NovaArchiveKey {
        cleared,
        collectibles,
        available,
        has_ability,
        ability: state.ability,
        health,
        chips,
        started_level: state.started_level,
        level: state.level,
        x: state.x / 16,
        y: state.y / 16,
        state_fingerprint,
    }
}

/// Strongest durable and mechanical rungs observed by a campaign.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaMilestones {
    /// Greatest completed-level count.
    pub cleared: u8,
    /// Greatest unlocked-level count.
    pub available: u8,
    /// Greatest durable collectible count.
    pub collectibles: u8,
    /// Whether any input acquired an ability.
    pub acquired_ability: bool,
}

/// First deterministic execution reaching each durable rung.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaMilestoneTimes {
    /// First execution that cleared a level.
    pub first_clear: Option<u64>,
    /// First execution that acquired a collectible.
    pub first_collectible: Option<u64>,
    /// First execution that acquired an ability.
    pub first_ability: Option<u64>,
}

/// First clean-reset input reaching each durable rung.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaMilestoneInputs {
    /// First input that cleared a level.
    pub first_clear: Option<NovaInput>,
    /// First input that acquired a collectible.
    pub first_collectible: Option<NovaInput>,
    /// First input that acquired an ability.
    pub first_ability: Option<NovaInput>,
}

/// Strongest lexicographic mechanical position seen at any emulated frame.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NovaProgressWatermark {
    /// Durable completed-level count.
    pub cleared: u8,
    /// Durable collectible count.
    pub collectibles: u8,
    /// Unlocked-level count.
    pub available: u8,
    /// Selected campaign level.
    pub started_level: u8,
    /// Internal map number.
    pub level: u8,
    /// Whole-pixel X position.
    pub x: u16,
    /// Whole-pixel Y position.
    pub y: u16,
}

/// Nova progress curve point.
pub type NovaArchiveProgressPoint = ProgressPoint<NovaMilestones, NovaProgressWatermark>;
/// Nova archive entry report.
pub type NovaArchiveEntryReport = ArchiveEntryReport<ButtonChord, NovaArchiveKey, NovaMilestones>;

/// Complete deterministic report for one Nova campaign.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaArchiveReport {
    /// Campaign seed.
    pub seed: u64,
    /// Admitted executions.
    pub executions: u64,
    /// Strongest durable milestones.
    pub milestones: NovaMilestones,
    /// Strongest per-frame mechanical progress.
    pub progress_watermark: NovaProgressWatermark,
    /// First execution reaching each durable rung.
    pub first_reached: NovaMilestoneTimes,
    /// First input reaching each durable rung.
    pub first_inputs: NovaMilestoneInputs,
    /// Best input under Nova's opaque progress/preference order.
    pub champion_input: NovaInput,
    /// Retained per-location representatives.
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<NovaArchiveEntryReport>,
    /// Fixed-interval deterministic progress curve.
    pub progress_curve: Vec<NovaArchiveProgressPoint>,
    /// Candidates admitted.
    pub retained: u64,
    /// Candidates rejected or superseded.
    pub rejected: u64,
    /// Terminal deaths observed.
    pub deaths: u64,
    /// Generic selector accounting.
    #[serde(default)]
    pub selector: SelectorAccounting,
}

/// Decode milestones from one state.
#[must_use]
pub fn milestones(state: NovaMechanicalState) -> NovaMilestones {
    NovaMilestones {
        cleared: state.cleared_count(),
        available: state.available_count(),
        collectibles: state.collectible_count(),
        acquired_ability: state.ability != 0,
    }
}

/// Merge strongest milestone fields.
pub fn merge_milestones(into: &mut NovaMilestones, from: NovaMilestones) {
    into.cleared = into.cleared.max(from.cleared);
    into.available = into.available.max(from.available);
    into.collectibles = into.collectibles.max(from.collectibles);
    into.acquired_ability |= from.acquired_ability;
}

/// Stable champion order owned by the Nova adapter.
#[must_use]
pub fn milestone_key(value: NovaMilestones) -> (u8, u8, u8, bool) {
    (
        value.cleared,
        value.collectibles,
        value.available,
        value.acquired_ability,
    )
}

/// Fold every action-interior observation into the progress watermark.
pub fn merge_progress_watermark(
    watermark: &mut NovaProgressWatermark,
    observations: &[NovaObservations],
) {
    for observation in observations {
        let state = observation.decoded;
        *watermark = (*watermark).max(NovaProgressWatermark {
            cleared: state.cleared_count(),
            collectibles: state.collectible_count(),
            available: state.available_count(),
            started_level: state.started_level,
            level: state.level,
            x: state.x,
            y: state.y,
        });
    }
}

/// Held-frame clock used by same-slot route replacement.
pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

/// Longest hold [`sample_chord`] can draw; the suffix time bound is a
/// multiple of it.
pub const LONGEST_HOLD_FRAMES: u8 = 120;

const DIRECTIONS: [u8; 9] = [0, 0x10, 0x20, 0x40, 0x80, 0x50, 0x90, 0x60, 0xa0];
const AB: [u8; 4] = [0, 0x01, 0x02, 0x03];

/// Draw one game-neutral controller chord from Nova's recorded vocabulary.
pub fn sample_chord(rand: &mut RomuDuoJrRand) -> Result<ButtonChord, Box<dyn Error>> {
    let direction = DIRECTIONS
        [rand.below(NonZeroUsize::new(DIRECTIONS.len()).ok_or("empty Nova direction vocabulary")?)];
    let buttons =
        direction | AB[rand.below(NonZeroUsize::new(AB.len()).ok_or("empty Nova A/B vocabulary")?)];
    let hold_frames = if rand.below(NonZeroUsize::new(2).ok_or("invalid duration odds")?) == 0 {
        u8::try_from(2 + rand.below(NonZeroUsize::new(11).ok_or("invalid short duration")?))?
    } else {
        u8::try_from(48 + rand.below(NonZeroUsize::new(73).ok_or("invalid long duration")?))?
    };
    Ok(ButtonChord::new(buttons, hold_frames))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::archive::first_depth_ord_disagreement;

    /// Nova ranks its resource fields ahead of position on purpose, so the
    /// key's ordering and its grouping disagree and the deepest-live report
    /// can move backwards across a level. That is a measured trade, not an
    /// oversight: preferring undamaged splice donors cleared level 25 in
    /// 104,751 executions where the grouping-consistent order had not
    /// cleared by 258,900. Pin the deviation so a future reorder is a
    /// deliberate re-measurement rather than a silent drift.
    #[test]
    fn key_ordering_deliberately_deviates_from_the_grouping() {
        let mut samples = Vec::new();
        for x in [0_u16, 3, 200] {
            for y in [0_u16, 7] {
                for health in [0_u8, 4] {
                    for has_ability in [false, true] {
                        for chips in [0_u8, 2] {
                            for cleared in [0_u8, 1] {
                                samples.push(NovaArchiveKey {
                                    state_fingerprint: 0,
                                    cleared,
                                    collectibles: 0,
                                    available: 1,
                                    started_level: 8,
                                    level: 8,
                                    x,
                                    y,
                                    has_ability,
                                    ability: 0,
                                    health,
                                    chips,
                                });
                            }
                        }
                    }
                }
            }
        }
        assert!(first_depth_ord_disagreement(&samples).is_some());
    }

    fn state(x: u16, health: u8, cleared: u8) -> NovaMechanicalState {
        let mut value = NovaMechanicalState {
            x,
            y: 64,
            health,
            ..NovaMechanicalState::default()
        };
        value.levels_cleared[0] = cleared;
        value
    }

    #[test]
    fn one_location_uses_opaque_resources_only_for_preference() {
        let weak = archive_key(state(100, 2, 0), &[0_u8; 16], 0);
        let strong = archive_key(state(100, 4, 1), &[0_u8; 16], 0);
        assert_eq!(weak.group(0), strong.group(0));
        assert_eq!(weak.group(1), strong.group(1));
        assert_eq!(strong.preference_cmp(weak), Ordering::Greater);
        assert_eq!(NovaArchiveKey::slot_capacity(), 1);
    }

    #[test]
    fn vocabulary_never_draws_start_or_select_or_conflicting_verticals() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        for _ in 0..1_000 {
            let chord = sample_chord(&mut rand).expect("draw chord");
            assert_eq!(chord.buttons & 0x0c, 0);
            assert_ne!(chord.buttons & 0x30, 0x30);
            assert_ne!(chord.buttons & 0xc0, 0xc0);
        }
    }
}
