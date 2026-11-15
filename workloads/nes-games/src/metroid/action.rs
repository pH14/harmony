// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, num::NonZeroUsize};

use machine::nes::ButtonChord;
use serde::{Deserialize, Serialize};

use crate::{
    chord::SHORT_HOLD_FRAMES,
    metroid::{
        archive::{CHORDS, chord_time},
        target::{JOYPAD_B, JOYPAD_SELECT, MetroidMechanicalState},
    },
    search::rand::RomuDuoJrRand,
};

pub const FIRE_ODDS: usize = 12;

pub const FIRE_WAIT_FRAMES: u8 = SHORT_HOLD_FRAMES.1;

pub const FIRE_PLANNED_FRAMES: u64 = 12;

const TOGGLE_FRAME_LIMIT: u32 = 8;

const SHOT_FRAME_LIMIT: u32 = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum MetroidAction {
    Chord(ButtonChord),
    FireMissiles(u32),
}

impl From<ButtonChord> for MetroidAction {
    fn from(chord: ButtonChord) -> Self {
        Self::Chord(chord)
    }
}

impl MetroidAction {
    pub fn draw(rand: &mut RomuDuoJrRand, previous: Option<&Self>) -> Result<Self, Box<dyn Error>> {
        let odds = NonZeroUsize::new(FIRE_ODDS).ok_or("fire odds are zero")?;
        if rand.below(odds) == 0 {
            return Ok(Self::FireMissiles(u32::try_from(rand.next_u64() >> 32)?));
        }
        let previous = previous.and_then(|action| match action {
            Self::Chord(chord) => Some(chord),
            Self::FireMissiles(_) => None,
        });
        Ok(Self::Chord(CHORDS.draw(rand, previous)?))
    }

    #[must_use]
    pub fn chord(&self) -> Option<ButtonChord> {
        match self {
            Self::Chord(chord) => Some(*chord),
            Self::FireMissiles(_) => None,
        }
    }

    #[must_use]
    pub fn planned_frames(&self) -> u64 {
        match self {
            Self::Chord(chord) => chord_time(chord),
            Self::FireMissiles(_) => FIRE_PLANNED_FRAMES,
        }
    }
}

#[must_use]
pub fn missile_count(held: u8, word: u32) -> u8 {
    let Some(held) = NonZeroUsize::new(usize::from(held)) else {
        return 0;
    };
    usize::try_from(word)
        .ok()
        .and_then(|word| u8::try_from(word % held + 1).ok())
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetroidReading {
    pub state: MetroidMechanicalState,
    pub missiles_armed: bool,
    pub missile_target: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Arm,
    Fire,
    Disarm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FireMissiles {
    remaining: u8,
    phase: Phase,
    phase_frames: u32,
    last_missiles: u8,
    pressed: bool,
}

impl FireMissiles {
    #[must_use]
    pub fn new(action: MetroidAction, reading: MetroidReading) -> Option<Self> {
        let MetroidAction::FireMissiles(word) = action else {
            return None;
        };
        let state = reading.state;
        if !state.in_play() || state.is_dead() || !reading.missile_target {
            return None;
        }
        let remaining = missile_count(state.missiles, word);
        (remaining > 0).then_some(Self {
            remaining,
            phase: Phase::Arm,
            phase_frames: 0,
            last_missiles: state.missiles,
            pressed: false,
        })
    }

    fn enter(&mut self, phase: Phase) {
        self.phase = phase;
        self.phase_frames = 0;
        self.pressed = false;
    }

    pub fn next(&mut self, reading: MetroidReading) -> Option<ButtonChord> {
        let state = reading.state;
        if !state.in_play() || state.is_dead() {
            return None;
        }
        let fired = self.last_missiles.saturating_sub(state.missiles);
        self.last_missiles = state.missiles;
        if fired > 0 {
            self.remaining = self.remaining.saturating_sub(fired);
            self.phase_frames = 0;
        }
        if self.phase == Phase::Arm && reading.missiles_armed {
            self.enter(Phase::Fire);
        }
        if self.phase == Phase::Fire
            && (self.remaining == 0 || state.missiles == 0 || !reading.missile_target)
        {
            self.enter(Phase::Disarm);
        }
        if self.phase == Phase::Disarm && !reading.missiles_armed {
            return None;
        }
        let limit = match self.phase {
            Phase::Arm | Phase::Disarm => TOGGLE_FRAME_LIMIT,
            Phase::Fire => SHOT_FRAME_LIMIT,
        };
        if self.phase_frames >= limit {
            if self.phase == Phase::Fire {
                self.enter(Phase::Disarm);
            } else {
                return None;
            }
        }
        let buttons = match self.phase {
            Phase::Arm | Phase::Disarm if self.pressed => 0,
            Phase::Arm | Phase::Disarm => {
                self.pressed = true;
                JOYPAD_SELECT
            }
            Phase::Fire => {
                self.pressed = !self.pressed;
                if self.pressed { JOYPAD_B } else { 0 }
            }
        };
        self.phase_frames += 1;
        Some(ButtonChord::new(buttons, 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metroid::target::{GAME_MODE_PLAYING, POSE_STANDING};

    fn reading(missiles: u8, armed: bool) -> MetroidReading {
        MetroidReading {
            state: MetroidMechanicalState {
                mode: GAME_MODE_PLAYING,
                health: 300,
                pose: POSE_STANDING,
                missiles,
                ..MetroidMechanicalState::default()
            },
            missiles_armed: armed,
            missile_target: true,
        }
    }

    struct Game {
        missiles: u8,
        armed: bool,
        held: u8,
        fires: bool,
        target_hits: Option<u8>,
    }

    impl Game {
        fn press(&mut self, buttons: u8) {
            if buttons & JOYPAD_SELECT != 0 && self.held & JOYPAD_SELECT == 0 {
                self.armed = !self.armed;
            }
            if self.fires
                && self.armed
                && self.missiles > 0
                && buttons & JOYPAD_B != 0
                && self.held & JOYPAD_B == 0
            {
                self.missiles -= 1;
                self.target_hits = self.target_hits.map(|hits| hits.saturating_sub(1));
            }
            self.held = buttons;
        }

        fn reading(&self) -> MetroidReading {
            let mut reading = reading(self.missiles, self.armed);
            reading.missile_target = self.target_hits != Some(0);
            reading
        }
    }

    fn run(word: u32, mut game: Game) -> (Vec<u8>, Game) {
        let mut pressed = Vec::new();
        if let Some(mut fire) = FireMissiles::new(MetroidAction::FireMissiles(word), game.reading())
        {
            while let Some(chord) = fire.next(game.reading()) {
                game.press(chord.buttons);
                pressed.push(chord.buttons);
            }
        }
        (pressed, game)
    }

    fn game(missiles: u8) -> Game {
        Game {
            missiles,
            armed: false,
            held: 0,
            fires: true,
            target_hits: None,
        }
    }

    #[test]
    fn the_word_picks_a_count_from_one_to_the_missiles_held() {
        assert_eq!(missile_count(0, 7), 0);
        let counts = (0..6)
            .map(|word| missile_count(3, word))
            .collect::<Vec<_>>();
        assert_eq!(counts, [1, 2, 3, 1, 2, 3]);
    }

    #[test]
    fn fire_arms_fires_the_count_and_disarms() {
        let (pressed, after) = run(2, game(10));
        assert_eq!(after.missiles, 7);
        assert!(!after.armed);
        assert_eq!(
            pressed,
            [
                JOYPAD_SELECT,
                JOYPAD_B,
                0,
                JOYPAD_B,
                0,
                JOYPAD_B,
                JOYPAD_SELECT
            ]
        );
    }

    #[test]
    fn fire_spends_every_missile_when_the_word_asks_for_all() {
        let (_, after) = run(4, game(5));
        assert_eq!(after.missiles, 0);
        assert!(!after.armed);
    }

    #[test]
    fn fire_starting_armed_skips_arming() {
        let mut start = game(4);
        start.armed = true;
        let (pressed, after) = run(0, start);
        assert_eq!(pressed, [JOYPAD_B, JOYPAD_SELECT]);
        assert_eq!(after.missiles, 3);
        assert!(!after.armed);
    }

    #[test]
    fn fire_that_cannot_shoot_gives_up_and_disarms() {
        let mut blocked = game(6);
        blocked.fires = false;
        let (pressed, after) = run(3, blocked);
        assert_eq!(after.missiles, 6);
        assert!(!after.armed);
        assert_eq!(
            pressed.len(),
            1 + usize::try_from(SHOT_FRAME_LIMIT).unwrap() + 1
        );
    }

    #[test]
    fn fire_with_no_missiles_or_outside_play_does_nothing() {
        assert_eq!(run(5, game(0)).0, Vec::<u8>::new());
        let mut dead = reading(5, false);
        dead.state.health = 0;
        assert!(FireMissiles::new(MetroidAction::FireMissiles(1), dead).is_none());
    }

    #[test]
    fn fire_with_nothing_in_the_room_that_takes_missiles_does_nothing() {
        let mut empty = game(5);
        empty.target_hits = Some(0);
        assert_eq!(run(4, empty).0, Vec::<u8>::new());
    }

    #[test]
    fn fire_stops_and_disarms_once_the_target_is_gone() {
        let mut door = game(10);
        door.target_hits = Some(2);
        let (pressed, after) = run(8, door);
        assert_eq!(after.missiles, 8);
        assert!(!after.armed);
        assert_eq!(
            pressed,
            [JOYPAD_SELECT, JOYPAD_B, 0, JOYPAD_B, JOYPAD_SELECT]
        );
    }

    #[test]
    fn draws_mix_fire_with_chords_without_select() {
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_3302);
        let mut fires = 0_u32;
        let mut previous = None;
        for _ in 0..12_000 {
            let action = MetroidAction::draw(&mut rand, previous.as_ref()).expect("draw");
            match action {
                MetroidAction::FireMissiles(_) => fires += 1,
                MetroidAction::Chord(chord) => assert_eq!(chord.buttons & JOYPAD_SELECT, 0),
            }
            previous = Some(action);
        }
        assert!((850..=1_150).contains(&fires), "fires {fires}");
    }
}
