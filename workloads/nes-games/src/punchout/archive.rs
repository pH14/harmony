// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{
    chord::{ChordVocabulary, Tap},
    punchout::target::{
        ButtonChord, FULL_HEALTH, HEALTH_BUCKET, MODE_FIGHT, MODE_ROUND_BREAK, MODE_TKO, MODE_WON,
        OPPONENT_COUNT, PunchOutInput, PunchOutMechanicalState, PunchOutObservations,
        PunchOutSnapshot,
    },
    search::archive::{
        Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
        entries_by_suffix,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;
pub const KEY_POLICY_IDENTIFIER: &str = "punchout_defeated_tier_opponent_round_health_8_action_knockdown_scene_timer_16_place_mac_health_hearts_stars_preference_v4";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

const SCENE_TIMER_BUCKET: u8 = 16;
const PHASE_FIGHT: u8 = 0;
const PHASE_ROUND_BREAK: u8 = 1;
const PHASE_WON: u8 = 2;
const PHASE_OTHER: u8 = 3;

pub type PunchOutArchive =
    Archive<ButtonChord, PunchOutArchiveKey, PunchOutMilestones, PunchOutSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PunchOutTier {
    pub defeated: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PunchOutArchiveKey {
    pub tier: PunchOutTier,
    pub opponent: u8,
    pub phase: u8,
    pub screen: u8,
    pub round: u8,
    pub opponent_health: u8,
    pub opponent_action: u8,
    pub round_knockdowns: u8,
    pub scene: u8,
    pub prompt: u8,
    pub scene_timer: u8,
    pub mac_health: u8,
    pub hearts: u8,
    pub stars: u8,
}

impl ArchiveKey for PunchOutArchiveKey {
    type Place = (u8, u8, u8, u8, u8, u8, u8, u8, u8, u8);
    type Progress = PunchOutTier;
    type Identity = (u8, u8, u8);

    fn place(self) -> Self::Place {
        (
            self.opponent,
            self.phase,
            self.screen,
            self.round,
            self.round_knockdowns,
            self.opponent_health / HEALTH_BUCKET,
            self.opponent_action,
            self.scene,
            self.prompt,
            self.scene_timer,
        )
    }

    fn progress(self) -> Self::Progress {
        self.tier
    }

    fn identity(self) -> Self::Identity {
        (self.opponent_health, self.mac_health, self.hearts)
    }

    fn capacity() -> usize {
        1
    }

    fn preferences() -> usize {
        1
    }

    fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
        (self.mac_health, self.hearts, self.stars).cmp(&(
            other.mac_health,
            other.hearts,
            other.stars,
        ))
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PunchOutChampionKey {
    defeated: u8,
    knockdowns: u8,
    damage: u8,
    mac_health: u8,
}

impl PunchOutArchiveKey {
    #[must_use]
    pub fn champion_key(self) -> PunchOutChampionKey {
        let fighting = self.phase == PHASE_FIGHT;
        PunchOutChampionKey {
            defeated: self.tier.defeated,
            knockdowns: self.round_knockdowns,
            damage: if fighting {
                FULL_HEALTH.saturating_sub(self.opponent_health)
            } else {
                0
            },
            mac_health: self.mac_health,
        }
    }
}

#[must_use]
pub fn tier(state: PunchOutMechanicalState) -> PunchOutTier {
    PunchOutTier {
        defeated: state.opponents_defeated(),
    }
}

fn phase(mode: u8) -> u8 {
    match mode {
        MODE_FIGHT => PHASE_FIGHT,
        MODE_ROUND_BREAK => PHASE_ROUND_BREAK,
        MODE_WON | MODE_TKO => PHASE_WON,
        _ => PHASE_OTHER,
    }
}

#[must_use]
pub fn archive_key(state: PunchOutMechanicalState) -> Option<PunchOutArchiveKey> {
    if state.terminal() {
        return None;
    }
    let fighting = state.fighting();
    Some(PunchOutArchiveKey {
        tier: tier(state),
        opponent: state.opponent,
        phase: phase(state.mode),
        screen: if fighting { 0 } else { state.screen },
        round: state.round,
        opponent_health: if fighting { state.opponent_health } else { 0 },
        opponent_action: if fighting { state.opponent_action } else { 0 },
        round_knockdowns: if fighting { state.round_knockdowns } else { 0 },
        scene: if fighting { 0 } else { state.scene },
        prompt: if fighting { 0 } else { state.prompt },
        scene_timer: if fighting {
            0
        } else {
            state.scene_timer / SCENE_TIMER_BUCKET
        },
        mac_health: state.mac_health,
        hearts: state.hearts,
        stars: state.stars,
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PunchOutMilestones {
    pub defeated: u8,
    pub knockdowns: u8,
    pub victory: bool,
    pub death: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PunchOutMilestoneTimes {
    pub first_defeats: [Option<u64>; OPPONENT_COUNT as usize],
    pub first_death: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PunchOutMilestoneInputs {
    pub first_defeats: [Option<PunchOutInput>; OPPONENT_COUNT as usize],
    pub first_death: Option<PunchOutInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PunchOutProgressWatermark {
    pub defeated: u8,
    pub knockdowns: u8,
    pub damage: u8,
}

pub type PunchOutArchiveProgressPoint =
    ProgressPoint<PunchOutMilestones, PunchOutProgressWatermark>;
pub type PunchOutArchiveEntryReport =
    ArchiveEntryReport<ButtonChord, PunchOutArchiveKey, PunchOutMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PunchOutArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: PunchOutMilestones,
    pub progress_watermark: PunchOutProgressWatermark,
    pub first_reached: PunchOutMilestoneTimes,
    pub first_inputs: PunchOutMilestoneInputs,
    pub champion_input: PunchOutInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<PunchOutArchiveEntryReport>,
    pub progress_curve: Vec<PunchOutArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(state: PunchOutMechanicalState) -> PunchOutMilestones {
    PunchOutMilestones {
        defeated: state.opponents_defeated(),
        knockdowns: if state.fighting() {
            state.round_knockdowns
        } else {
            0
        },
        victory: state.victory(),
        death: state.dead(),
    }
}

#[must_use]
pub fn milestones_from_observation(observation: &PunchOutObservations) -> PunchOutMilestones {
    milestones(observation.decoded)
}

pub fn merge_milestones(into: &mut PunchOutMilestones, from: PunchOutMilestones) {
    if from.defeated > into.defeated {
        into.knockdowns = from.knockdowns;
    } else if from.defeated == into.defeated {
        into.knockdowns = into.knockdowns.max(from.knockdowns);
    }
    into.defeated = into.defeated.max(from.defeated);
    into.victory |= from.victory;
    into.death |= from.death;
}

#[must_use]
pub fn milestone_key(value: PunchOutMilestones) -> (u8, bool, u8, bool) {
    (
        value.defeated,
        value.victory,
        value.knockdowns,
        !value.death,
    )
}

#[must_use]
pub fn progress_point(state: PunchOutMechanicalState) -> PunchOutProgressWatermark {
    let fighting = state.fighting();
    PunchOutProgressWatermark {
        defeated: state.opponents_defeated(),
        knockdowns: if fighting { state.round_knockdowns } else { 0 },
        damage: if fighting {
            FULL_HEALTH.saturating_sub(state.opponent_health)
        } else {
            0
        },
    }
}

pub fn merge_progress_watermark(
    watermark: &mut PunchOutProgressWatermark,
    observations: &[PunchOutObservations],
) {
    for observation in observations {
        *watermark = (*watermark).max(progress_point(observation.decoded));
    }
}

pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

const START: u8 = 0x08;

pub const HELD: [u8; 8] = [0x00, 0x01, 0x02, 0x11, 0x12, 0x40, 0x80, 0x20];

pub const CHORDS: ChordVocabulary = ChordVocabulary {
    held: &HELD,
    tap: Some(Tap {
        buttons: START,
        odds: 12,
        hold_frames: (2, 4),
    }),
    short_hold: (2, 8),
    long_hold: (9, 32),
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::rand::RomuDuoJrRand;

    fn state(opponent_health: u8, mac_health: u8) -> PunchOutMechanicalState {
        PunchOutMechanicalState {
            mode: MODE_FIGHT,
            round: 1,
            mac_health,
            opponent_health,
            hearts: 20,
            opponent_action: 1,
            ..PunchOutMechanicalState::default()
        }
    }

    #[test]
    fn opponent_health_is_an_eight_point_place_and_the_tier_is_opponents_defeated() {
        let fresh = archive_key(state(0x60, 0x60)).unwrap();
        let hit = archive_key(state(0x58, 0x60)).unwrap();
        assert_ne!(fresh.place(), hit.place());
        assert_eq!(fresh.progress(), hit.progress());
        let hurt = archive_key(state(0x58, 0x20)).unwrap();
        assert_eq!(hurt.place(), hit.place());
        assert_eq!(hurt.progress(), hit.progress());
        assert_eq!(hit.preference_cmp(0, hurt), Ordering::Greater);
    }

    #[test]
    fn states_differing_only_in_mac_health_or_hearts_share_a_tier() {
        let full = state(0x58, 0x60);
        let mut low_health = full;
        low_health.mac_health = 0x08;
        let mut low_hearts = full;
        low_hearts.hearts = 0;
        let mut low_stars = full;
        low_stars.stars = 3;
        let full = archive_key(full).unwrap();
        for other in [low_health, low_hearts, low_stars] {
            let other = archive_key(other).unwrap();
            assert_eq!(full.progress(), other.progress());
            assert_eq!(full.place(), other.place());
            assert_ne!(full, other);
        }
        assert_eq!(
            full.preference_cmp(0, archive_key(low_health).unwrap()),
            Ordering::Greater
        );
        assert_eq!(
            full.preference_cmp(0, archive_key(low_hearts).unwrap()),
            Ordering::Greater
        );
        assert_ne!(full.identity(), archive_key(low_health).unwrap().identity());
        assert_ne!(full.identity(), archive_key(low_hearts).unwrap().identity());
    }

    #[test]
    fn a_defeated_opponent_outranks_every_state_of_the_previous_opponent() {
        let mut next = state(0x60, 0x08);
        next.opponent = 1;
        next.hearts = 0;
        let healthy = archive_key(state(0x10, 0x60)).unwrap();
        assert!(archive_key(next).unwrap().progress() > healthy.progress());
    }

    #[test]
    fn screens_between_fights_are_places_without_fight_fields() {
        let mut won = state(0, 0x40);
        won.mode = MODE_WON;
        won.opponent = 1;
        let mut profile = won;
        profile.screen = 2;
        profile.opponent_action = 9;
        let (won, profile) = (archive_key(won).unwrap(), archive_key(profile).unwrap());
        assert_ne!(won.place(), profile.place());
        assert_eq!(profile.opponent_action, 0);
        assert_eq!(won.progress().defeated, 1);
    }

    #[test]
    fn knockout_and_technical_knockout_share_the_won_phase() {
        let mut knockout = state(0, 0x40);
        knockout.mode = MODE_WON;
        knockout.opponent = 1;
        let mut technical = knockout;
        technical.mode = MODE_TKO;
        assert_eq!(
            archive_key(knockout).unwrap().place(),
            archive_key(technical).unwrap().place()
        );
    }

    #[test]
    fn the_cutscene_timer_separates_places_between_fights() {
        let mut early = state(0, 0x40);
        early.mode = MODE_TKO;
        early.opponent = 3;
        early.scene = 0x0c;
        early.scene_timer = 0xfe;
        let mut late = early;
        late.scene_timer = 0x40;
        assert_ne!(
            archive_key(early).unwrap().place(),
            archive_key(late).unwrap().place()
        );
        let mut fighting = state(0x60, 0x60);
        fighting.scene_timer = 0x40;
        assert_eq!(archive_key(fighting).unwrap().scene_timer, 0);
    }

    #[test]
    fn terminal_states_have_no_key_and_count_as_deaths() {
        let mut down = state(0x30, 0);
        down.stop = 2;
        assert_eq!(archive_key(down), None);
        assert!(milestones(down).death);
    }

    #[test]
    fn knockdowns_restart_with_each_defeated_opponent() {
        let mut aggregate = PunchOutMilestones::default();
        let mut knocked = state(0, 0x60);
        knocked.round_knockdowns = 2;
        merge_milestones(&mut aggregate, milestones(knocked));
        assert_eq!(aggregate.knockdowns, 2);
        let mut next = state(0x60, 0x60);
        next.opponent = 1;
        merge_milestones(&mut aggregate, milestones(next));
        assert_eq!((aggregate.defeated, aggregate.knockdowns), (1, 0));
        merge_milestones(&mut aggregate, milestones(knocked));
        assert_eq!((aggregate.defeated, aggregate.knockdowns), (1, 0));
    }

    #[test]
    fn vocabulary_draws_punches_dodges_blocks_and_short_start_taps() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        let mut starts = 0;
        for _ in 0..1_000 {
            let chord = CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
            previous = Some(chord);
            assert_eq!(chord.buttons & 0x04, 0);
            assert!(HELD.contains(&(chord.buttons & !START)));
            assert!(chord.hold_frames <= 32);
            if chord.buttons & START != 0 {
                starts += 1;
            }
        }
        assert!((40..=140).contains(&starts), "start taps: {starts}");
    }
}
