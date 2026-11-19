// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use crate::{
    chord::{ChordVocabulary, Tap},
    search::assertion::{Assertion, Declarations, Number},
    workload::{Game, Moves, Never, Step},
};
use machine::nes::ButtonChord;

pub const OPPONENT_COUNT: u8 = 14;
pub const FULL_HEALTH: u8 = 0x60;
pub const INITIAL_HEARTS: u8 = 20;
pub const MAX_HEARTS: u8 = 99;
pub const MODE_FIGHT: u8 = 0x01;
pub const MODE_ROUND_BREAK: u8 = 0xff;
pub const MODE_WON: u8 = 0xfe;
pub const MODE_TKO: u8 = 0xfc;
pub const MODE_LOST: u8 = 0xfd;
pub const MODE_TKO_LOST: u8 = 0xfb;
pub const MODE_WON_FA: u8 = 0xfa;
pub const SPIN_SERIES_ACTION: u8 = 26;
pub const PHASE_FIGHT: u8 = 0;
pub const PHASE_ROUND_BREAK: u8 = 1;
pub const PHASE_WON: u8 = 2;
pub const PHASE_OTHER: u8 = 3;
const STOP_MAC_DOWN: u8 = 2;
const OPPONENT_ACTION_ID: u8 = 0x7f;

const MODE: usize = 0x00;
const OPPONENT: usize = 0x01;
const SCREEN: usize = 0x02;
const STOP: usize = 0x05;
const ROUND: usize = 0x06;
const SCENE: usize = 0x0d;
const PROMPT: usize = 0x12;
const SCENE_TIMER: usize = 0x13;
const OPPONENT_ACTION: usize = 0x90;
const OPPONENT_STEP: usize = 0x96;
const HEARTS_TENS: usize = 0x323;
const HEARTS_ONES: usize = 0x324;
const STARS: usize = 0x342;
const MAC_HEALTH: usize = 0x391;
const OPPONENT_HEALTH: usize = 0x398;
const ROUND_KNOCKDOWNS: usize = 0x3ca;
const START: u8 = 0x08;

pub const HELD: [u8; 8] = [0x00, 0x01, 0x02, 0x11, 0x12, 0x40, 0x80, 0x20];

const DEFEATED: [&str; OPPONENT_COUNT as usize] = [
    "defeated_glass_joe",
    "defeated_von_kaiser",
    "defeated_piston_honda",
    "defeated_don_flamenco",
    "defeated_king_hippo",
    "defeated_great_tiger",
    "defeated_bald_bull",
    "defeated_piston_honda_2",
    "defeated_soda_popinski",
    "defeated_bald_bull_2",
    "defeated_don_flamenco_2",
    "defeated_mr_sandman",
    "defeated_super_macho_man",
    "defeated_mr_dream",
];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PunchOutState {
    pub mode: u8,
    pub opponent: u8,
    pub screen: u8,
    pub stop: u8,
    pub round: u8,
    pub mac_health: u8,
    pub opponent_health: u8,
    pub hearts: u8,
    pub stars: u8,
    pub round_knockdowns: u8,
    pub opponent_action: u8,
    pub opponent_step: u8,
    pub scene: u8,
    pub prompt: u8,
    pub scene_timer: u8,
}

impl PunchOutState {
    #[must_use]
    pub fn fighting(self) -> bool {
        self.mode == MODE_FIGHT
    }

    #[must_use]
    pub fn knocked_down(self) -> bool {
        self.fighting() && self.stop == STOP_MAC_DOWN
    }

    #[must_use]
    pub fn dead(self) -> bool {
        self.knocked_down() || matches!(self.mode, MODE_LOST | MODE_TKO_LOST)
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.opponent == OPPONENT_COUNT && matches!(self.mode, MODE_WON | MODE_TKO | MODE_WON_FA)
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead() || self.victory()
    }

    #[must_use]
    pub fn opponents_defeated(self) -> u8 {
        if self.victory() {
            OPPONENT_COUNT
        } else {
            self.opponent.min(OPPONENT_COUNT - 1)
        }
    }

    #[must_use]
    pub fn phase(self) -> u8 {
        match self.mode {
            MODE_FIGHT => PHASE_FIGHT,
            MODE_ROUND_BREAK => PHASE_ROUND_BREAK,
            MODE_WON | MODE_TKO => PHASE_WON,
            _ => PHASE_OTHER,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PunchOut;

fn byte(ram: &[u8], address: usize) -> u8 {
    ram.get(address).copied().unwrap_or(0)
}

fn value(name: &'static str, value: u8, min: u8, max: u8) -> Assertion {
    Assertion::Value {
        name,
        value: Number::new(i64::from(value), i64::from(min), i64::from(max)),
    }
}

fn byte_value(name: &'static str, value: u8) -> Assertion {
    Assertion::Value {
        name,
        value: Number::byte(value),
    }
}

fn flag(name: &'static str, value: bool) -> Assertion {
    Assertion::Value {
        name,
        value: Number::flag(value),
    }
}

impl Game for PunchOut {
    type State = PunchOutState;
    type Move = Never;

    const NAME: &'static str = "punch-out";
    const STRATEGY: &'static str = include_str!("punchout.toml");
    const CHORDS: ChordVocabulary = ChordVocabulary {
        held: &HELD,
        tap: Some(Tap {
            buttons: START,
            odds: 12,
            hold_frames: (2, 4),
        }),
        short_hold: (2, 8),
        long_hold: (9, 32),
    };

    fn setup(&self) -> Vec<ButtonChord> {
        let wait = |frames: usize| (0..frames).map(|_| ButtonChord::new(0, 1));
        wait(300)
            .chain([ButtonChord::new(START, 2)])
            .chain(wait(400))
            .chain([ButtonChord::new(START, 2)])
            .chain(wait(1100))
            .collect()
    }

    fn genesis(&self, state: &PunchOutState) -> Result<(), String> {
        let state = *state;
        let valid = state.fighting()
            && state.opponent == 0
            && state.round == 1
            && state.mac_health == FULL_HEALTH
            && state.opponent_health == FULL_HEALTH
            && state.hearts == INITIAL_HEARTS
            && state.stars == 0
            && !state.terminal();
        if valid {
            Ok(())
        } else {
            Err(format!(
                "Punch-Out!! setup did not reach the first round against Glass Joe: {state:?}"
            ))
        }
    }

    fn decode(
        &self,
        _: Option<&PunchOutState>,
        _previous: Option<&PunchOutState>,
        ram: &[u8],
    ) -> PunchOutState {
        let digit = |address| byte(ram, address).min(9);
        PunchOutState {
            mode: byte(ram, MODE),
            opponent: byte(ram, OPPONENT),
            screen: byte(ram, SCREEN),
            stop: byte(ram, STOP),
            round: byte(ram, ROUND),
            mac_health: byte(ram, MAC_HEALTH),
            opponent_health: byte(ram, OPPONENT_HEALTH),
            hearts: digit(HEARTS_TENS) * 10 + digit(HEARTS_ONES),
            stars: byte(ram, STARS),
            round_knockdowns: byte(ram, ROUND_KNOCKDOWNS),
            opponent_action: byte(ram, OPPONENT_ACTION) & OPPONENT_ACTION_ID,
            opponent_step: byte(ram, OPPONENT_STEP),
            scene: byte(ram, SCENE),
            prompt: byte(ram, PROMPT),
            scene_timer: byte(ram, SCENE_TIMER),
        }
    }

    fn declare(&self, state: &PunchOutState, out: &mut impl Declarations) {
        let state = *state;
        out.declare(byte_value("mode", state.mode));
        out.declare(value("phase", state.phase(), PHASE_FIGHT, PHASE_OTHER));
        out.declare(flag("fighting", state.fighting()));
        out.declare(byte_value("opponent", state.opponent));
        out.declare(value(
            "defeated",
            state.opponents_defeated(),
            0,
            OPPONENT_COUNT,
        ));
        out.declare(byte_value("screen", state.screen));
        out.declare(byte_value("stop", state.stop));
        out.declare(byte_value("round", state.round));
        out.declare(byte_value("round_knockdowns", state.round_knockdowns));
        out.declare(byte_value("opponent_health", state.opponent_health));
        out.declare(value(
            "opponent_action",
            state.opponent_action,
            0,
            OPPONENT_ACTION_ID,
        ));
        out.declare(byte_value("opponent_step", state.opponent_step));
        out.declare(byte_value("scene", state.scene));
        out.declare(byte_value("prompt", state.prompt));
        out.declare(byte_value("scene_timer", state.scene_timer));
        out.declare(byte_value("mac_health", state.mac_health));
        out.declare(value("hearts", state.hearts, 0, MAX_HEARTS));
        out.declare(byte_value("stars", state.stars));
        out.declare(flag("dead", state.dead()));
        out.declare(Assertion::Sometimes {
            name: "victory",
            condition: state.victory(),
        });
        out.declare(Assertion::Sometimes {
            name: "death",
            condition: state.dead(),
        });
        out.declare(Assertion::Sometimes {
            name: "knockdown",
            condition: state.fighting() && state.round_knockdowns > 0,
        });
        for (mark, name) in DEFEATED.iter().enumerate() {
            out.declare(Assertion::Sometimes {
                name,
                condition: usize::from(state.opponents_defeated()) > mark,
            });
        }
    }

    fn moves(&self, _state: &PunchOutState) -> Moves<Never> {
        Moves::none()
    }

    fn play(&self, mv: Never, _state: &PunchOutState) -> Option<Step<Never>> {
        match mv {}
    }

    fn move_frames(&self, mv: Never) -> u64 {
        match mv {}
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::*;
    use crate::{
        search::{
            archive::ArchiveKey,
            assertion::{Accumulator, AssertionKey},
            rand::RomuDuoJrRand,
        },
        workload::{Lowered, NesObservation, Satisfied},
    };
    use machine::nes::WRAM_SIZE;

    fn fighting_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[MODE] = MODE_FIGHT;
        wram[ROUND] = 1;
        wram[MAC_HEALTH] = FULL_HEALTH;
        wram[OPPONENT_HEALTH] = FULL_HEALTH;
        wram[HEARTS_TENS] = 2;
        wram[OPPONENT_ACTION] = 0x81;
        wram
    }

    fn decode(wram: &[u8]) -> PunchOutState {
        PunchOut.decode(None, None, wram)
    }

    fn lowered() -> Lowered<PunchOut> {
        Lowered::new(PunchOut, PunchOut::STRATEGY).unwrap()
    }

    fn observe(state: PunchOutState) -> NesObservation<PunchOutState> {
        let lowered = lowered();
        lowered.checked_record(&state).unwrap();
        lowered.observe(0, state, &mut Accumulator::default())
    }

    fn key(state: PunchOutState) -> AssertionKey {
        observe(state).key.unwrap()
    }

    fn marks(satisfied: Satisfied) -> Vec<String> {
        lowered()
            .sometimes()
            .iter()
            .enumerate()
            .filter(|(index, _)| satisfied.holds(*index))
            .map(|(_, name)| name.clone())
            .collect()
    }

    fn state(opponent_health: u8, mac_health: u8) -> PunchOutState {
        PunchOutState {
            mode: MODE_FIGHT,
            round: 1,
            mac_health,
            opponent_health,
            hearts: INITIAL_HEARTS,
            opponent_action: 1,
            ..PunchOutState::default()
        }
    }

    #[test]
    fn the_decoder_reads_hearts_as_two_decimal_digits_and_masks_the_action_start_bit() {
        let mut wram = fighting_wram();
        wram[HEARTS_TENS] = 1;
        wram[HEARTS_ONES] = 7;
        wram[OPPONENT_STEP] = 255;
        let state = decode(&wram);
        assert_eq!(state.hearts, 17);
        assert_eq!(state.opponent_step, 255);
        assert_eq!(state.opponent_action, 0x01);
        wram[HEARTS_TENS] = 0xff;
        wram[HEARTS_ONES] = 0xff;
        assert_eq!(decode(&wram).hearts, MAX_HEARTS);
        assert_eq!(decode(&[]), PunchOutState::default());
    }

    #[test]
    fn genesis_requires_the_first_round_against_glass_joe_at_full_health() {
        assert!(PunchOut.genesis(&decode(&fighting_wram())).is_ok());
        for (address, value) in [(MAC_HEALTH, FULL_HEALTH - 1), (OPPONENT, 1), (STARS, 1)] {
            let mut wram = fighting_wram();
            wram[address] = value;
            assert!(PunchOut.genesis(&decode(&wram)).is_err());
        }
    }

    #[test]
    fn a_knockdown_of_mac_and_a_lost_fight_are_rejected_deaths() {
        let mut down = state(0x30, 0);
        down.stop = STOP_MAC_DOWN;
        let observed = observe(down);
        assert!(observed.key.is_none() && observed.rejected == Some(0) && !observed.goal);
        assert!(observed.stops());
        assert_eq!(marks(observed.satisfied), ["death"]);
        for mode in [MODE_LOST, MODE_TKO_LOST] {
            let mut lost = state(0x30, 0x10);
            lost.mode = mode;
            lost.opponent = 3;
            let observed = observe(lost);
            assert!(observed.key.is_none() && observed.rejected == Some(0) && !observed.goal);
        }
        let mut standing = state(0, 0x30);
        standing.stop = 1;
        assert!(!observe(standing).stops());
    }

    #[test]
    fn a_final_fight_win_is_the_goal_in_every_win_mode() {
        for mode in [MODE_WON, MODE_TKO, MODE_WON_FA] {
            let mut won = state(0, 0x30);
            won.mode = mode;
            won.opponent = OPPONENT_COUNT;
            let observed = observe(won);
            assert!(observed.key.is_none() && observed.rejected.is_none() && observed.goal);
            assert!(observed.stops());
        }
        let mut lost = state(0, 0x30);
        lost.mode = MODE_LOST;
        lost.opponent = OPPONENT_COUNT;
        let observed = observe(lost);
        assert!(!observed.goal && observed.rejected == Some(0));
        let mut last = state(0x60, 0x60);
        last.opponent = OPPONENT_COUNT;
        assert!(!observe(last).stops());
    }

    #[test]
    fn the_attract_demo_opponent_value_counts_thirteen_defeated_and_is_not_the_goal() {
        let mut demo = state(0x60, 0x60);
        demo.opponent = 0x13;
        assert_eq!(demo.opponents_defeated(), OPPONENT_COUNT - 1);
        demo.mode = MODE_WON;
        assert_eq!(demo.opponents_defeated(), OPPONENT_COUNT - 1);
        assert!(!observe(demo).goal);
    }

    #[test]
    fn opponent_health_is_an_eight_point_place_and_mac_health_is_identity() {
        let fresh = key(state(0x60, 0x60));
        let hit = key(state(0x58, 0x60));
        let nudged = key(state(0x5f, 0x60));
        assert_ne!(fresh.place(), hit.place());
        assert_eq!(hit.place(), nudged.place());
        assert_ne!(hit.identity(), nudged.identity());
        assert_eq!(fresh.progress(), hit.progress());
        let hurt = key(state(0x58, 0x20));
        assert_eq!(hurt.place(), hit.place());
        assert_ne!(hurt.identity(), hit.identity());
    }

    #[test]
    fn the_opponent_step_separates_places_only_during_the_spin_series() {
        let mut spinning = state(0x58, 0x60);
        spinning.opponent_action = SPIN_SERIES_ACTION;
        spinning.opponent_step = 2;
        let mut later = spinning;
        later.opponent_step = 0;
        let mut wrapped = spinning;
        wrapped.opponent_step = 6;
        assert_ne!(key(spinning).place(), key(later).place());
        assert_eq!(key(spinning).place(), key(wrapped).place());
        let mut idle = spinning;
        idle.opponent_action = 1;
        let mut idle_later = later;
        idle_later.opponent_action = 1;
        assert_eq!(key(idle).place(), key(idle_later).place());
    }

    #[test]
    fn screens_between_fights_are_places_without_fight_fields() {
        let mut won = state(0, 0x40);
        won.mode = MODE_WON;
        won.opponent = 1;
        let mut profile = won;
        profile.screen = 2;
        let mut acting = profile;
        acting.opponent_action = 9;
        acting.opponent_health = 0x30;
        assert_ne!(key(won).place(), key(profile).place());
        assert_eq!(key(profile), key(acting));
        let mut technical = won;
        technical.mode = MODE_TKO;
        assert_eq!(key(won).place(), key(technical).place());
        let mut late = won;
        late.mode = MODE_WON_FA;
        assert_ne!(key(won).place(), key(late).place());
    }

    #[test]
    fn the_cutscene_timer_separates_places_between_fights_in_sixteen_frame_steps() {
        let mut early = state(0, 0x40);
        early.mode = MODE_TKO;
        early.opponent = 3;
        early.scene = 0x0c;
        early.scene_timer = 0xfe;
        let mut late = early;
        late.scene_timer = 0x40;
        let mut close = early;
        close.scene_timer = 0xf1;
        assert_ne!(key(early).place(), key(late).place());
        assert_eq!(key(early).place(), key(close).place());
        let mut fighting = state(0x60, 0x60);
        let quiet = key(fighting);
        fighting.scene_timer = 0x40;
        fighting.scene = 3;
        fighting.screen = 4;
        assert_eq!(key(fighting), quiet);
    }

    #[test]
    fn a_defeated_opponent_outranks_every_state_of_the_previous_opponent() {
        let mut next = state(0x60, 0x08);
        next.opponent = 1;
        next.hearts = 0;
        let healthy = key(state(0x10, 0x60));
        assert!(key(next).progress() > healthy.progress());
        let mut won = state(0, 0x40);
        won.mode = MODE_WON;
        won.opponent = 1;
        assert_eq!(key(won).progress(), key(next).progress());
    }

    #[test]
    fn more_mac_health_then_hearts_then_stars_is_preferred() {
        let base = state(0x58, 0x40);
        let mut healthy = base;
        healthy.mac_health = 0x48;
        healthy.hearts = 0;
        let mut hearty = base;
        hearty.hearts = 30;
        let mut starry = base;
        starry.stars = 3;
        let base_key = key(base);
        assert_eq!(key(starry).identity(), base_key.identity());
        assert_eq!(key(starry).preference_cmp(0, base_key), Ordering::Greater);
        assert_eq!(
            key(hearty).preference_cmp(0, key(starry)),
            Ordering::Greater
        );
        assert_eq!(
            key(healthy).preference_cmp(0, key(hearty)),
            Ordering::Greater
        );
        assert_eq!(lowered().compiled.shape().preferences, 1);
    }

    #[test]
    fn named_progress_marks_every_defeated_opponent_and_knockdowns() {
        let mut names = vec![
            "victory".to_owned(),
            "death".to_owned(),
            "knockdown".to_owned(),
        ];
        names.extend(DEFEATED.iter().map(|name| (*name).to_owned()));
        assert_eq!(lowered().sometimes(), names);
        let mut later = state(0x30, 0x60);
        later.opponent = 2;
        later.round_knockdowns = 1;
        assert_eq!(
            marks(observe(later).satisfied),
            ["knockdown", "defeated_glass_joe", "defeated_von_kaiser"]
        );
        let mut won = state(0, 0x60);
        won.mode = MODE_WON;
        won.opponent = OPPONENT_COUNT;
        won.round_knockdowns = 2;
        let satisfied = marks(observe(won).satisfied);
        assert_eq!(satisfied.len(), 1 + DEFEATED.len());
        assert_eq!(satisfied[0], "victory");
    }

    #[test]
    fn vocabulary_taps_start_and_excludes_select() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        let mut starts = 0;
        for _ in 0..1_000 {
            let chord = PunchOut::CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
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

    #[test]
    fn setup_tape_presses_start_twice_without_other_buttons() {
        let tape = PunchOut.setup();
        assert_eq!(tape.iter().filter(|chord| chord.buttons != 0).count(), 2);
        assert!(tape.iter().all(|chord| chord.buttons & !START == 0));
    }
}
