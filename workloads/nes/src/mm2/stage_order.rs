// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use super::target::{ButtonChord, Mm2Stage};

pub const STAGE_ORDER_FIELD: &str = "experimental_stage_order_v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StageOrder([u8; 8]);

impl StageOrder {
    pub fn parse(text: &str) -> Result<Self, Box<dyn Error>> {
        let stages = text
            .split(',')
            .map(Mm2Stage::parse)
            .collect::<Result<Vec<_>, _>>()?;
        let mut mask = 0_u8;
        let mut order = [0_u8; 8];
        if stages.len() != order.len() {
            return Err("stage order requires each of the eight robot stages exactly once".into());
        }
        for (destination, stage) in order.iter_mut().zip(stages) {
            let number = stage.number();
            if number >= 8 || mask & (1 << number) != 0 {
                return Err("stage order cannot repeat stages or include Wily stages".into());
            }
            mask |= 1 << number;
            *destination = number;
        }
        Ok(Self(order))
    }

    pub fn identifier(&self) -> String {
        self.0
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn menu_action(&self, inventory: u8, cursor: u8, held: u8) -> ButtonChord {
        let positions = [
            (0, 1),
            (1, 0),
            (2, 1),
            (0, 0),
            (2, 0),
            (1, 2),
            (0, 2),
            (2, 2),
        ];
        let destination = self
            .0
            .iter()
            .find(|stage| inventory & (1 << **stage) == 0)
            .map_or((1, 1), |stage| positions[usize::from(*stage)]);
        let cursors = [
            (1, 1),
            (0, 0),
            (1, 0),
            (2, 0),
            (2, 1),
            (2, 2),
            (1, 2),
            (0, 2),
            (0, 1),
        ];
        let here = cursors[usize::from(cursor)];
        let button = if here.0 < destination.0 {
            0x80
        } else if here.0 > destination.0 {
            0x40
        } else if here.1 < destination.1 {
            0x20
        } else if here.1 > destination.1 {
            0x10
        } else {
            0x08
        };
        ButtonChord::new(if held & button != 0 { 0 } else { button }, 4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_order_requires_a_permutation() {
        assert!(StageOrder::parse("flash,crash,metal,wood,air,bubble,heat,quick").is_ok());
        assert!(StageOrder::parse("5,7,6,2,1,3,0,0").is_err());
        assert!(StageOrder::parse("5,7,6,2,1,3,0,8").is_err());
        assert!(StageOrder::parse("5,7").is_err());
    }

    #[test]
    fn menu_guidance_skips_owned_weapons_and_releases_buttons() {
        let order = StageOrder::parse("5,7,6,2,1,3,0,4").unwrap();
        assert_eq!(order.menu_action(0, 0, 0).buttons, 0x20);
        assert_eq!(order.menu_action(0, 6, 0).buttons, 0x08);
        assert_eq!(order.menu_action(0, 6, 0x08).buttons, 0);
        assert_eq!(order.menu_action(0x20, 6, 0).buttons, 0x80);
        assert_eq!(order.menu_action(0xa0, 0, 0).buttons, 0x40);
        assert_eq!(order.menu_action(0xff, 0, 0).buttons, 0x08);
        assert_eq!(order.menu_action(0xff, 5, 0).buttons, 0x40);
    }
    #[test]
    #[ignore = "requires the supplied MM2 ROM and QuickNES core"]
    fn every_prescribed_first_stage_enters_gameplay_from_power_on() {
        use crate::{mm2::target::Mm2Target, target::Target};
        use sha2::{Digest, Sha256};
        use std::{env, fs, path::Path};
        let rom = fs::read(env::var("HARMONY_MM2_ROM").unwrap()).unwrap();
        let core = env::var("HARMONY_QUICKNES_CORE").unwrap();
        let hash = format!("{:x}", Sha256::digest(fs::read(&core).unwrap()));
        for first in 0..8 {
            let order = StageOrder::parse(
                &(0..8)
                    .map(|offset| ((first + offset) % 8).to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            )
            .unwrap();
            let mut target =
                Mm2Target::from_rom_bytes_whole_game(&rom, Path::new(&core), &hash).unwrap();
            assert!(target.stage_selection_cursor().is_some());
            let mut entered = false;
            for _ in 0..160 {
                let state = target.mechanical_state();
                if state.current_bank == 14
                    && state.health == 28
                    && matches!(state.player_state, 3 | 4 | 6)
                {
                    assert_eq!(state.stage, first);
                    entered = true;
                    break;
                }
                let action = target
                    .stage_selection_cursor()
                    .map_or(ButtonChord::new(0, 60), |(cursor, held)| {
                        order.menu_action(state.weapons_obtained, cursor, held)
                    });
                target.apply(&action);
            }
            assert!(entered, "failed to enter stage {first}");
        }
    }
}
