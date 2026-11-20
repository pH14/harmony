// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use crate::{
    chord::{ChordVocabulary, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES},
    search::assertion::{Assertion, Declarations, Number},
    smb::{
        plan,
        target::{
            BOOT_WALK, ROOM_IDENTITY_BYTES, smb_area_is_loading, smb_camera_pixels, smb_is_playing,
            smb_is_victory, smb_player_has_control, smb_player_is_doomed, smb_player_position,
        },
    },
    workload::{Game, Moves, Never, Step},
};
use machine::nes::{ButtonChord, WRAM_SIZE};

pub const WARPS_STRATEGY: &str = include_str!("smb_warps.toml");
pub const LONG_HOLD_FRAMES: (u8, u8) = (96, 120);
pub const BOOT_IDLE_FRAMES: usize = 152;
pub const WARP_DROP: u16 = 17;
const BAND_MAX: i64 = 32;
pub const LEVELS: u8 = 32;

const PLAYER_Y: usize = 0x00ce;
const PLAYER_ENGINE_STATE: usize = 0x000e;
const PLAYER_KILLED_STATE: u8 = 0x0b;
const PLAYER_VERTICAL_PAGE: usize = 0x00b5;
const PLAYER_BELOW_PLAY_AREA_PAGE: u8 = 2;
const WORLD: usize = 0x075f;
const LEVEL: usize = 0x075c;
const GAME_TIMER: usize = 0x07f8;
const LOOP_CORRECT_PASSES: usize = 0x06d9;
const LOOP_PASSES: usize = 0x06da;
const OPERATING_MODE: usize = 0x0770;

const REACHED: [&str; LEVELS as usize] = [
    "reached_1_1",
    "reached_1_2",
    "reached_1_3",
    "reached_1_4",
    "reached_2_1",
    "reached_2_2",
    "reached_2_3",
    "reached_2_4",
    "reached_3_1",
    "reached_3_2",
    "reached_3_3",
    "reached_3_4",
    "reached_4_1",
    "reached_4_2",
    "reached_4_3",
    "reached_4_4",
    "reached_5_1",
    "reached_5_2",
    "reached_5_3",
    "reached_5_4",
    "reached_6_1",
    "reached_6_2",
    "reached_6_3",
    "reached_6_4",
    "reached_7_1",
    "reached_7_2",
    "reached_7_3",
    "reached_7_4",
    "reached_8_1",
    "reached_8_2",
    "reached_8_3",
    "reached_8_4",
];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbState {
    pub world: u8,
    pub level: u8,
    pub raw_world: u8,
    pub raw_level: u8,
    pub area: [u8; 2],
    pub progress: u16,
    pub screen_x: u8,
    pub y_bucket: u8,
    pub loop_on_path: bool,
    pub time_bucket: u8,
    pub clock: u16,
    pub warp_zone: u8,
    pub vine: u16,
    pub victory: bool,
    pub dead: bool,
    pub doomed: bool,
    pub vine_out_of_reach: bool,
    pub playing: bool,
    pub control: bool,
    pub operating_mode: u8,
    pub page: u8,
    pub band: u16,
}

impl SmbState {
    #[must_use]
    pub fn level_index(self) -> u8 {
        self.world.saturating_mul(4).saturating_add(self.level)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SuperMarioBros;

fn wram(ram: &[u8]) -> [u8; WRAM_SIZE] {
    let mut wram = [0; WRAM_SIZE];
    let len = ram.len().min(WRAM_SIZE);
    wram[..len].copy_from_slice(&ram[..len]);
    wram
}

fn screen_x_bucket(wram: &[u8; WRAM_SIZE]) -> u8 {
    let player_x = (smb_player_position(wram).x as u16 as i16).max(0) as u32;
    let screen_x = player_x.saturating_sub(smb_camera_pixels(wram));
    u8::try_from(screen_x.min(255) / 16).unwrap_or(15)
}

fn game_clock(wram: &[u8; WRAM_SIZE]) -> u16 {
    wram[GAME_TIMER..GAME_TIMER + 3]
        .iter()
        .fold(0, |clock, digit| clock * 10 + u16::from(*digit))
}

fn value(name: &'static str, value: i64, min: i64, max: i64) -> Assertion {
    Assertion::Value {
        name,
        value: Number::new(value, min, max),
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

impl Game for SuperMarioBros {
    type State = SmbState;
    type Move = Never;

    const NAME: &'static str = "super-mario-bros";
    const STRATEGY: &'static str = include_str!("smb.toml");
    const CHORDS: ChordVocabulary = ChordVocabulary {
        held: &NES_PRESSABLE_BUTTON_MASKS,
        tap: None,
        short_hold: SHORT_HOLD_FRAMES,
        long_hold: LONG_HOLD_FRAMES,
    };

    fn setup(&self) -> Vec<ButtonChord> {
        BOOT_WALK
            .into_iter()
            .chain((0..BOOT_IDLE_FRAMES).map(|_| ButtonChord::new(0, 1)))
            .collect()
    }

    fn genesis(&self, state: &SmbState) -> Result<(), String> {
        let valid = state.playing
            && state.level_index() == 0
            && !state.dead
            && !state.victory
            && state.progress == 0;
        if valid {
            Ok(())
        } else {
            Err(format!(
                "Super Mario Bros. setup did not reach play in 1-1: {state:?}"
            ))
        }
    }

    fn rooted_genesis(&self, state: &SmbState) -> Result<(), String> {
        if state.playing && !state.dead && !state.victory {
            Ok(())
        } else {
            Err(format!(
                "Super Mario Bros. root input did not end in live play: {state:?}"
            ))
        }
    }

    fn decode(
        &self,
        parent: Option<&SmbState>,
        _previous: Option<&SmbState>,
        ram: &[u8],
    ) -> SmbState {
        let wram = wram(ram);
        let loading = smb_area_is_loading(&wram);
        let area = ROOM_IDENTITY_BYTES.map(|offset| wram[offset]);
        let (raw_world, raw_level) = (wram[WORLD], wram[LEVEL]);
        let progress = if loading {
            0
        } else {
            u16::try_from(smb_camera_pixels(&wram) / 16).unwrap_or(u16::MAX)
        };
        let screen_x = if loading { 0 } else { screen_x_bucket(&wram) };
        let (world, level) = match parent {
            Some(parent) if parent.area == area => (parent.world, parent.level),
            _ => (raw_world, raw_level),
        };
        let arrived = u8::try_from(progress / 16).unwrap_or(u8::MAX);
        let (page, carried) = match parent {
            Some(parent) if (parent.world, parent.level) == (world, level) => {
                if parent.area != area {
                    (arrived, parent.band)
                } else if parent.progress >= progress.saturating_add(WARP_DROP) {
                    (arrived, 0)
                } else {
                    (parent.page, parent.band)
                }
            }
            _ => (arrived, 0),
        };
        let band = plan::band(progress.saturating_add(u16::from(screen_x))).max(carried);
        let dead = wram[PLAYER_ENGINE_STATE] == PLAYER_KILLED_STATE
            || wram[PLAYER_VERTICAL_PAGE] >= PLAYER_BELOW_PLAY_AREA_PAGE;
        SmbState {
            world,
            level,
            raw_world,
            raw_level,
            area,
            progress,
            screen_x,
            y_bucket: wram[PLAYER_Y] / 16,
            loop_on_path: wram[LOOP_CORRECT_PASSES] == wram[LOOP_PASSES],
            time_bucket: wram[GAME_TIMER],
            clock: game_clock(&wram),
            warp_zone: plan::warp_zone(&wram),
            vine: plan::vine_step(&wram),
            victory: smb_is_victory(&wram),
            dead,
            doomed: !dead && smb_player_is_doomed(&wram),
            vine_out_of_reach: plan::vine_out_of_reach(&wram),
            playing: smb_is_playing(&wram),
            control: smb_player_has_control(&wram),
            operating_mode: wram[OPERATING_MODE],
            page,
            band,
        }
    }

    fn declare(&self, state: &SmbState, out: &mut impl Declarations) {
        let state = *state;
        out.declare(byte_value("world", state.world));
        out.declare(byte_value("level", state.level));
        out.declare(byte_value("raw_world", state.raw_world));
        out.declare(byte_value("raw_level", state.raw_level));
        out.declare(byte_value("area_type", state.area[0]));
        out.declare(byte_value("area_pointer", state.area[1]));
        out.declare(value("progress", i64::from(state.progress), 0, 2_047));
        out.declare(value("screen_x", i64::from(state.screen_x), 0, 15));
        out.declare(value("y_bucket", i64::from(state.y_bucket), 0, 15));
        out.declare(flag("loop_on_path", state.loop_on_path));
        out.declare(byte_value("time_bucket", state.time_bucket));
        out.declare(value("clock", i64::from(state.clock), 0, 28_305));
        out.declare(byte_value("warp_zone", state.warp_zone));
        out.declare(value(
            "vine",
            i64::from(state.vine),
            0,
            i64::from(plan::VINE_AREA),
        ));
        out.declare(byte_value("operating_mode", state.operating_mode));
        out.declare(flag("control", state.control));
        out.declare(value("page", i64::from(state.page), 0, 127));
        out.declare(value("band", i64::from(state.band), 0, BAND_MAX));
        out.declare(flag("dead", state.dead));
        out.declare(flag("doomed", state.doomed));
        out.declare(flag("vine_out_of_reach", state.vine_out_of_reach));
        out.declare(Assertion::Sometimes {
            name: "victory",
            condition: state.victory,
        });
        for (index, name) in REACHED.iter().enumerate() {
            out.declare(Assertion::Sometimes {
                name,
                condition: state.playing && usize::from(state.level_index()) >= index,
            });
        }
    }

    fn moves(&self, _state: &SmbState) -> Moves<Never> {
        Moves::none()
    }

    fn play(&self, mv: Never, _state: &SmbState) -> Option<Step<Never>> {
        match mv {}
    }

    fn move_frames(&self, mv: Never) -> u64 {
        match mv {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::Lowered;

    #[test]
    fn a_root_must_end_in_live_play_anywhere_in_the_game() {
        let playing = SmbState {
            world: 3,
            playing: true,
            progress: 40,
            ..SmbState::default()
        };
        assert!(SuperMarioBros.rooted_genesis(&playing).is_ok());
        assert!(SuperMarioBros.genesis(&playing).is_err());
        for state in [
            SmbState {
                playing: false,
                ..playing
            },
            SmbState {
                dead: true,
                ..playing
            },
            SmbState {
                victory: true,
                ..playing
            },
        ] {
            assert!(SuperMarioBros.rooted_genesis(&state).is_err());
        }
    }

    fn ram(area: [u8; 2], camera: u16, world: u8, level: u8) -> [u8; WRAM_SIZE] {
        let mut ram = [0; WRAM_SIZE];
        ram[OPERATING_MODE] = 1;
        ram[0x0772] = 3;
        ram[ROOM_IDENTITY_BYTES[0]] = area[0];
        ram[ROOM_IDENTITY_BYTES[1]] = area[1];
        ram[0x071a] = (camera >> 8) as u8;
        ram[0x071c] = camera as u8;
        ram[WORLD] = world;
        ram[LEVEL] = level;
        ram
    }

    #[test]
    fn both_routes_compile_against_the_catalog() {
        for strategy in [SuperMarioBros::STRATEGY, WARPS_STRATEGY] {
            let lowered = Lowered::new(SuperMarioBros, strategy).expect("strategy");
            assert_eq!(lowered.compiled.shape().capacity, 2);
            assert_eq!(lowered.compiled.shape().preferences, 1);
        }
    }

    #[test]
    fn the_level_page_and_band_follow_the_parent_and_a_warp_restarts_the_page() {
        let game = SuperMarioBros;
        let castle = game.decode(None, None, &ram([3, 5], 4_000, 0, 3));
        assert_eq!((castle.page, castle.band), (15, 3));
        let next = game.decode(Some(&castle), Some(&castle), &ram([3, 5], 4_000, 1, 0));
        assert_eq!((next.world, next.level), (0, 3));
        let loaded = game.decode(Some(&next), Some(&next), &ram([1, 0], 0, 1, 0));
        assert_eq!(
            (loaded.world, loaded.level, loaded.page, loaded.band),
            (1, 0, 0, 0)
        );
        let ahead = game.decode(Some(&loaded), Some(&loaded), &ram([1, 0], 2_000, 1, 0));
        assert_eq!((ahead.page, ahead.band), (0, 1));
        let bonus = game.decode(Some(&ahead), Some(&ahead), &ram([0, 2], 0, 1, 0));
        assert_eq!((bonus.page, bonus.band), (0, 1));
        let warped = game.decode(Some(&ahead), Some(&ahead), &ram([1, 0], 1_000, 1, 0));
        assert_eq!((warped.page, warped.band), (3, 0));
        let back = game.decode(
            Some(&ahead),
            Some(&warped),
            &ram([1, 0], 2_000 - 16 * 16, 1, 0),
        );
        assert_eq!((back.page, back.band), (0, 1));
    }
}
