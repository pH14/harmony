// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    progress::NamedProgress,
    target::{Mm2MechanicalState, Mm2Observations, Mm2Scene, WEAPON_ENERGY_BYTES},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Anchor {
    stage: u8,
    screen: u8,
    room: u8,
    scene: Mm2Scene,
    inventory: u8,
    platforms: u8,
    castle_clears: u8,
    refights: u8,
    refight_boss: u8,
    shell: bool,
    boobeam_targets: u16,
    boss_damage: u8,
}

impl From<Mm2MechanicalState> for Anchor {
    fn from(state: Mm2MechanicalState) -> Self {
        Self {
            stage: state.stage,
            screen: state.screen,
            room: state.room,
            scene: state.scene,
            inventory: state.weapons_obtained,
            platforms: state.platforms,
            castle_clears: state.castle_clears,
            refights: state.refighting_mask,
            refight_boss: state.refight_boss,
            shell: state.wily_machine_shell_broken,
            boobeam_targets: state.boobeam_targets,
            boss_damage: if state.current_bank == 0x0e {
                state.boss_damage()
            } else {
                0
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Stock {
    anchor: Anchor,
    health: Option<(u8, u8, u16)>,
    lives: Option<(u8, u8, u16)>,
    energies: [Option<(u8, u8, u8)>; WEAPON_ENERGY_BYTES],
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct StockedProgress {
    milestones: BTreeMap<String, Stock>,
}

impl StockedProgress {
    pub fn anchor(&mut self, name: &str, observation: &Mm2Observations) {
        self.milestones.entry(name.to_owned()).or_insert(Stock {
            anchor: observation.decoded.into(),
            health: None,
            lives: None,
            energies: [None; WEAPON_ENERGY_BYTES],
        });
    }

    pub fn observe(&mut self, observation: &Mm2Observations) -> Vec<String> {
        if observation.dead
            || observation.decoded.health == 0
            || observation.decoded.scene == Mm2Scene::Ending
        {
            return Vec::new();
        }
        let state = observation.decoded;
        let anchor = state.into();
        let mut improved = Vec::new();
        let mut reached = None;
        for (name, stock) in &mut self.milestones {
            if stock.anchor != anchor
                || !reached
                    .get_or_insert_with(|| NamedProgress::reached(observation))
                    .contains(name)
            {
                continue;
            }
            let health = (state.health, state.lives, state.weapon_energy);
            if stock.health.is_none_or(|best| health > best) {
                stock.health = Some(health);
                improved.push(format!("{name}-health"));
            }
            let lives = (state.lives, state.health, state.weapon_energy);
            if stock.lives.is_none_or(|best| lives > best) {
                stock.lives = Some(lives);
                improved.push(format!("{name}-lives"));
            }
            for (index, (best, energy)) in stock
                .energies
                .iter_mut()
                .zip(state.weapon_energies)
                .enumerate()
            {
                let quantity = (energy, state.health, state.lives);
                if best.is_none_or(|best| quantity > best) {
                    *best = Some(quantity);
                    improved.push(format!("{name}-energy-{index}"));
                }
            }
        }
        improved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation() -> Mm2Observations {
        Mm2Observations {
            decoded: Mm2MechanicalState {
                stage: 8,
                room: 36,
                health: 4,
                lives: 1,
                scene: Mm2Scene::Gameplay,
                current_bank: 14,
                player_state: 3,
                weapons_obtained: 255,
                ..Default::default()
            },
            frame_count: 100,
            changed_indices: Vec::new(),
            dead: false,
            fall_run: 0,
            log_line: String::new(),
        }
    }

    #[test]
    fn a_scrolling_room_byte_cannot_supply_a_playable_room_stock_record() {
        let mut entry = observation();
        entry.decoded.stage = 3;
        entry.decoded.room = 1;
        entry.decoded.screen = 1;
        entry.decoded.scene = Mm2Scene::Unknown;
        entry.decoded.weapons_obtained = 0;
        entry.decoded.health = 27;
        entry.decoded.player_state = 6;
        let mut stock = StockedProgress::default();
        stock.anchor("bubble_room_1", &entry);
        assert_eq!(stock.observe(&entry).len(), 14);
        let mut scrolling = entry.clone();
        scrolling.decoded.current_bank = 10;
        scrolling.decoded.health = 28;
        assert!(!NamedProgress::reached(&scrolling).contains(&"bubble_room_1".to_owned()));
        assert!(stock.observe(&scrolling).is_empty());
        scrolling.decoded.current_bank = 14;
        assert!(
            stock
                .observe(&scrolling)
                .contains(&"bubble_room_1-energy-0".to_owned())
        );
    }

    #[test]
    fn richer_later_rooms_and_progress_cannot_replace_an_entrance_tape() {
        let mut stock = StockedProgress::default();
        let entry = observation();
        stock.anchor("wily1_room_36", &entry);
        assert_eq!(stock.observe(&entry).len(), 14);
        for mutate in [
            |state: &mut Mm2MechanicalState| state.room += 1,
            |state: &mut Mm2MechanicalState| state.stage += 1,
            |state: &mut Mm2MechanicalState| state.castle_clears += 1,
            |state: &mut Mm2MechanicalState| state.refighting_mask = 1,
            |state: &mut Mm2MechanicalState| {
                state.boss_phase = 3;
                state.boss_health = 1;
                state.player_state = 3;
            },
            |state: &mut Mm2MechanicalState| state.weapons_obtained = 254,
        ] {
            let mut later = entry.clone();
            later.decoded.health = 28;
            mutate(&mut later.decoded);
            assert!(stock.observe(&later).is_empty());
        }
        let mut richer = entry.clone();
        richer.decoded.health = 28;
        richer.decoded.x = 200;
        assert!(
            stock
                .observe(&richer)
                .contains(&"wily1_room_36-health".to_owned())
        );
        assert!(stock.observe(&richer).is_empty());
    }

    #[test]
    fn an_award_transition_does_not_freeze_stock_at_the_previous_boss_phase() {
        let mut transition = observation();
        transition.decoded.scene = Mm2Scene::Unknown;
        transition.decoded.current_bank = 13;
        transition.decoded.boss_phase = 5;
        transition.decoded.player_state = 11;
        let mut stock = StockedProgress::default();
        stock.anchor("wily1_entered", &transition);
        stock.observe(&transition);
        let mut arrival = transition;
        arrival.decoded.health = 28;
        arrival.decoded.current_bank = 14;
        arrival.decoded.boss_phase = 0;
        arrival.decoded.player_state = 3;
        assert!(
            stock
                .observe(&arrival)
                .contains(&"wily1_entered-health".to_owned())
        );
    }

    #[test]
    fn individual_weapon_records_survive_checkpointing_and_ignore_deaths() {
        let mut stock = StockedProgress::default();
        let mut entry = observation();
        stock.anchor("wily1_room_36", &entry);
        stock.observe(&entry);
        let bytes = postcard::to_allocvec(&stock).unwrap();
        let mut stock: StockedProgress = postcard::from_bytes(&bytes).unwrap();
        entry.decoded.weapon_energies[5] = 28;
        entry.dead = true;
        assert!(stock.observe(&entry).is_empty());
        entry.dead = false;
        assert_eq!(stock.observe(&entry), ["wily1_room_36-energy-5"]);
    }
}
