// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::nes::ButtonChord;
use serde::{Deserialize, Serialize};

use crate::{
    chord::ChordVocabulary,
    famidash::{
        game::{
            A_BUTTON, Famidash, FamidashState, GAME_STATE_LEVEL_SELECT, LEVEL, X_MAX, decode,
            level_select_tape,
        },
        levels::{Side, Stage},
    },
    search::{
        assertion::{Assertion, Declarations, Number},
        choice::ChoiceSite,
    },
    workload::{EVERY_OPTION, Game, Moves, Offer, Play, Step},
};

pub const LUCKY_DRAW_LEVEL: u8 = 24;
pub const WAIT_SITE: u16 = 0;
pub const START: u16 = 0;
pub const WAIT_ONE: u16 = 1;

const START_FRAMES: u8 = 40;
const PLAY_FRAMES: u64 = 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wait {
    Start,
    StartWait,
    Play,
    Idle(u8),
}

impl Wait {
    const fn frames(self) -> u64 {
        match self {
            Self::Start => 1 + START_FRAMES as u64 + PLAY_FRAMES,
            Self::StartWait => START_FRAMES as u64 + PLAY_FRAMES,
            Self::Play => PLAY_FRAMES,
            Self::Idle(frames) => frames as u64,
        }
    }
}

const WAITS: [(u16, Play<Wait>); 8] = [
    (START, Play::Move(Wait::Start)),
    (1, Play::Move(Wait::Idle(1))),
    (2, Play::Move(Wait::Idle(2))),
    (3, Play::Move(Wait::Idle(4))),
    (4, Play::Move(Wait::Idle(8))),
    (5, Play::Move(Wait::Idle(16))),
    (6, Play::Move(Wait::Idle(32))),
    (7, Play::Move(Wait::Idle(64))),
];

const WAIT_LABELS: [&str; 8] = ["start", "1", "2", "4", "8", "16", "32", "64"];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LuckyDrawState {
    pub famidash: FamidashState,
    pub menu_wait: u32,
}

impl LuckyDrawState {
    #[must_use]
    pub fn at_menu(self) -> bool {
        self.famidash.game_state == GAME_STATE_LEVEL_SELECT && self.famidash.gameplay.is_none()
    }

    #[must_use]
    pub fn resolved(self) -> bool {
        self.famidash.dead || self.famidash.complete()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LuckyDraw {
    pub famidash: Famidash<3>,
}

impl Default for LuckyDraw {
    fn default() -> Self {
        Self {
            famidash: Famidash::new(Stage {
                side: Side::C,
                level: LUCKY_DRAW_LEVEL,
                goal: LUCKY_DRAW_LEVEL,
            }),
        }
    }
}

impl Game for LuckyDraw {
    type State = LuckyDrawState;
    type Move = Wait;

    const NAME: &'static str = "famidash_lucky_draw";
    const STRATEGY: &'static str = include_str!("lucky_draw.toml");
    const CHORDS: ChordVocabulary = <Famidash<3> as Game>::CHORDS;

    fn setup(&self) -> Vec<ButtonChord> {
        level_select_tape(self.famidash.stage.group().menu)
    }

    fn setup_pokes(&self) -> Vec<(usize, u8)> {
        vec![(LEVEL, self.famidash.stage.level)]
    }

    fn genesis_id(&self) -> String {
        format!(
            "famidash-{}-level-{}-level-select-wait-v1",
            self.famidash.stage.side.id(),
            self.famidash.stage.level
        )
    }

    fn genesis(&self, state: &LuckyDrawState) -> Result<(), String> {
        if state.at_menu() && state.famidash.level == self.famidash.stage.level {
            Ok(())
        } else {
            Err(format!(
                "Famidash setup did not reach the level select at level {}: {state:?}",
                self.famidash.stage.level
            ))
        }
    }

    fn decode(&self, previous: Option<&LuckyDrawState>, ram: &[u8]) -> LuckyDrawState {
        let famidash = decode(self.famidash.stage, ram);
        let menu_wait = previous.map_or(0, |previous| {
            let waited = previous.at_menu() && famidash.game_state == GAME_STATE_LEVEL_SELECT;
            previous
                .menu_wait
                .saturating_add(u32::from(waited))
                .min(u32::try_from(X_MAX).unwrap_or(u32::MAX))
        });
        LuckyDrawState {
            famidash,
            menu_wait,
        }
    }

    fn declare(&self, state: &LuckyDrawState, out: &mut impl Declarations) {
        self.famidash.declare(&state.famidash, out);
        out.declare(Assertion::Value {
            name: "menu",
            value: Number::flag(state.at_menu()),
        });
        out.declare(Assertion::Value {
            name: "menu_wait",
            value: Number::new(i64::from(state.menu_wait), 0, X_MAX),
        });
    }

    fn moves(&self, state: &LuckyDrawState) -> Moves<Wait> {
        if state.at_menu() {
            Moves::offer(Offer {
                site: WAIT_SITE,
                default: WAIT_ONE,
                options: &WAITS,
                offered: EVERY_OPTION,
            })
        } else if state.resolved() {
            Moves::none()
        } else {
            Moves::forced(Wait::Play)
        }
    }

    fn move_sites(&self) -> Vec<ChoiceSite> {
        vec![ChoiceSite::new("wait", WAIT_LABELS.map(str::to_owned))]
    }

    fn play(&self, mv: Wait, state: &LuckyDrawState) -> Option<Step<Wait>> {
        Some(match mv {
            Wait::Start => Step {
                chord: ButtonChord::new(A_BUTTON, 1),
                rest: Some(Wait::StartWait),
            },
            Wait::StartWait => Step {
                chord: ButtonChord::new(0, START_FRAMES),
                rest: Some(Wait::Play),
            },
            Wait::Play if state.resolved() || state.at_menu() => return None,
            Wait::Play => Step {
                chord: ButtonChord::new(0, 1),
                rest: Some(Wait::Play),
            },
            Wait::Idle(frames) => Step {
                chord: ButtonChord::new(0, 1),
                rest: (frames > 1).then(|| Wait::Idle(frames - 1)),
            },
        })
    }

    fn move_frames(&self, mv: Wait) -> u64 {
        mv.frames()
    }

    fn answer_frames(site: u16, label: u16) -> u64 {
        match WAITS
            .iter()
            .find(|(offered, _)| site == WAIT_SITE && *offered == label)
        {
            Some((_, Play::Move(Wait::Idle(frames)))) => u64::from(*frames),
            Some((_, Play::Move(Wait::Start))) => 1 + u64::from(START_FRAMES),
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        famidash::game::{GAME_STATE_PLAYING, Gameplay},
        workload::{Lowered, chord_time, move_answer, move_chord},
    };

    fn menu(menu_wait: u32) -> LuckyDrawState {
        LuckyDrawState {
            famidash: FamidashState {
                game_state: GAME_STATE_LEVEL_SELECT,
                level: LUCKY_DRAW_LEVEL,
                side: Side::C,
                goal: LUCKY_DRAW_LEVEL,
                ..FamidashState::default()
            },
            menu_wait,
        }
    }

    fn playing(dead: bool) -> LuckyDrawState {
        LuckyDrawState {
            famidash: FamidashState {
                game_state: GAME_STATE_PLAYING,
                dead,
                gameplay: Some(Gameplay::default()),
                ..menu(0).famidash
            },
            ..menu(0)
        }
    }

    #[test]
    fn the_level_select_offers_the_wait_and_gameplay_is_played_to_its_outcome() {
        let game = LuckyDraw::default();
        let offer = game.moves(&menu(0)).offer.expect("an offer at the menu");
        assert_eq!((offer.site, offer.default), (WAIT_SITE, WAIT_ONE));
        assert_eq!(offer.labels().len(), WAIT_LABELS.len());
        let level = game.moves(&playing(false));
        assert!(level.offer.is_none());
        assert_eq!(level.forced, Some(Wait::Play));
        let dead = game.moves(&playing(true));
        assert!(dead.offer.is_none() && dead.forced.is_none());
    }

    #[test]
    fn the_start_plays_on_until_the_attempt_dies() {
        let game = LuckyDraw::default();
        let start = game.play(Wait::Start, &menu(0)).expect("a start press");
        assert_eq!(start.rest, Some(Wait::StartWait));
        let wait = game
            .play(Wait::StartWait, &menu(0))
            .expect("the start wait");
        assert_eq!(wait.rest, Some(Wait::Play));
        let step = game
            .play(Wait::Play, &playing(false))
            .expect("an idle frame");
        assert_eq!(step.chord, ButtonChord::new(0, 1));
        assert_eq!(step.rest, Some(Wait::Play));
        assert!(game.play(Wait::Play, &playing(true)).is_none());
        assert!(game.move_frames(Wait::Start) > PLAY_FRAMES);
    }

    #[test]
    fn a_move_chord_names_its_wait_and_anything_else_waits_one_frame() {
        let offer = LuckyDraw::default().moves(&menu(0)).offer.unwrap();
        let chord = move_chord(WAIT_SITE, 7).unwrap();
        assert_eq!(move_answer(&chord), Some((WAIT_SITE, 7)));
        assert_eq!(offer.chosen(&chord), Some(Wait::Idle(64)));
        assert_eq!(
            offer.chosen(&move_chord(WAIT_SITE, START).unwrap()),
            Some(Wait::Start)
        );
        assert_eq!(
            offer.chosen(&ButtonChord::new(A_BUTTON, 9)),
            Some(Wait::Idle(1))
        );
        assert_eq!(
            offer.chosen(&move_chord(WAIT_SITE, 40).unwrap()),
            Some(Wait::Idle(1))
        );
        assert_eq!(move_answer(&ButtonChord::new(0, 120)), None);
    }

    #[test]
    fn every_route_to_a_wait_costs_the_frames_it_waits() {
        let cost = |labels: &[u16]| -> u64 {
            labels
                .iter()
                .map(|label| chord_time::<LuckyDraw>(&move_chord(WAIT_SITE, *label).unwrap()))
                .sum()
        };
        assert_eq!(cost(&[7]), 64);
        assert_eq!(cost(&[1; 64]), 64);
        assert_eq!(cost(&[6, 5, 4, 3, 2, 1, 1]), 64);
        assert_eq!(cost(&[START]), 1 + u64::from(START_FRAMES));
        assert_eq!(chord_time::<LuckyDraw>(&ButtonChord::new(A_BUTTON, 9)), 9);
    }

    #[test]
    fn a_chord_at_the_level_select_is_kept_as_the_wait_it_plays() {
        let game = LuckyDraw::default();
        let offer = game.moves(&menu(5)).offer.unwrap();
        let played = offer.played(&ButtonChord::new(A_BUTTON, 9)).unwrap();
        assert_eq!(move_answer(&played), Some((WAIT_SITE, WAIT_ONE)));
        assert_eq!(chord_time::<LuckyDraw>(&played), 1);
        let wait = move_chord(WAIT_SITE, 7).unwrap();
        assert_eq!(offer.played(&wait).unwrap(), wait);
        let unoffered = offer.played(&move_chord(WAIT_SITE, 40).unwrap()).unwrap();
        assert_eq!(move_answer(&unoffered), Some((WAIT_SITE, WAIT_ONE)));
    }

    #[test]
    fn the_records_catalog_names_the_chord_sites_and_the_wait() {
        let lowered = Lowered::new(LuckyDraw::default(), LuckyDraw::STRATEGY).unwrap();
        assert_ne!(lowered.records_catalog().unwrap(), lowered.catalog);
        let sites = lowered.choice_sites();
        assert_eq!(sites.len(), LuckyDraw::CHORDS.sites().len() + 1);
        assert_eq!(sites.last().map(|site| site.name.as_str()), Some("wait"));
    }

    #[test]
    fn an_idle_wait_plays_one_frame_at_a_time() {
        let game = LuckyDraw::default();
        let mut mv = Wait::Idle(3);
        let mut frames = 0;
        while let Some(step) = game.play(mv, &menu(0)) {
            assert_eq!(step.chord, ButtonChord::new(0, 1));
            frames += 1;
            match step.rest {
                Some(next) => mv = next,
                None => break,
            }
        }
        assert_eq!(frames, game.move_frames(Wait::Idle(3)));
    }

    #[test]
    fn the_strategy_keys_each_menu_wait_in_and_after_the_level_select_and_rejects_a_death() {
        let lowered = Lowered::new(LuckyDraw::default(), LuckyDraw::STRATEGY).expect("compiles");
        let key = |state: &LuckyDrawState| {
            lowered
                .observe(
                    0,
                    *state,
                    &mut crate::search::assertion::Accumulator::default(),
                )
                .key
        };
        let first = key(&menu(5)).expect("a menu key");
        let second = key(&menu(6)).expect("a menu key");
        assert_ne!(first.place, second.place);
        let level = |menu_wait: u32| LuckyDrawState {
            famidash: FamidashState {
                game_state: GAME_STATE_PLAYING,
                gameplay: Some(Gameplay {
                    x: 300,
                    ..Gameplay::default()
                }),
                ..menu(menu_wait).famidash
            },
            menu_wait,
        };
        let early = key(&level(5)).expect("a level key");
        let late = key(&level(6)).expect("a level key");
        assert_ne!(early.place, late.place);
        let dead = LuckyDrawState {
            famidash: FamidashState {
                game_state: 2,
                dead: true,
                ..menu(0).famidash
            },
            ..menu(0)
        };
        assert!(
            lowered
                .observe(
                    0,
                    dead,
                    &mut crate::search::assertion::Accumulator::default()
                )
                .rejected
                .is_some()
        );
    }
}
