// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    metroid::target::MetroidMechanicalState,
    search::plan::{self, StageProgress},
};

const BRINSTAR: u8 = 0x10;
const NORFAIR: u8 = 0x11;
const KRAID: u8 = 0x12;
const TOURIAN: u8 = 0x13;
const RIDLEY: u8 = 0x14;

const MARU_MARI: u8 = 0x10;
const BOMBS: u8 = 0x01;

const ENERGY_UNIT: u16 = 10;
const BRINSTAR_TOUCH: u16 = 8;
const TOUCHES_BEFORE_BRINSTAR_ROUTE: u16 = 8;
const KRAID_HIDEOUT_TOUCH: u16 = 20;
const TOUCHES_BEFORE_KRAID: u16 = 4;
const TOUCHES_LEAVING_KRAID: u16 = 6;
const NORFAIR_TOUCH: u16 = 10;
const TOUCHES_BEFORE_NORFAIR_ROUTE: u16 = 8;
const RIDLEY_HIDEOUT_TOUCH: u16 = 24;
const TOUCHES_BEFORE_RIDLEY: u16 = 4;
const TOUCHES_BEFORE_TOURIAN: u16 = 12;
const MISSILES_BEFORE_TOURIAN: u8 = 80;

pub type Cell = (u8, u8, u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Done {
    Equipment(u8),
    MissileCapacity,
    RouteEnd,
    Energy(u16),
    Missiles(u8),
    Kraid,
    Ridley,
    MotherBrain,
    Ending,
    Never,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Objective {
    Route(&'static [Cell]),
    Energy,
    Missiles,
    BossDamage,
    None,
}

#[derive(Clone, Copy, Debug)]
pub struct Stage {
    pub name: &'static str,
    pub done: Done,
    pub objective: Objective,
}

const fn b(x: u8, y: u8) -> Cell {
    (BRINSTAR, x, y)
}

const fn n(x: u8, y: u8) -> Cell {
    (NORFAIR, x, y)
}

const fn k(x: u8, y: u8) -> Cell {
    (KRAID, x, y)
}

const fn r(x: u8, y: u8) -> Cell {
    (RIDLEY, x, y)
}

const fn t(x: u8, y: u8) -> Cell {
    (TOURIAN, x, y)
}

const TO_MARU_MARI: &[Cell] = &[b(3, 14), b(2, 14)];

const TO_MISSILES: &[Cell] = &[
    b(2, 14),
    b(3, 14),
    b(4, 14),
    b(5, 14),
    b(6, 14),
    b(7, 14),
    b(8, 14),
    b(9, 14),
    b(10, 14),
    b(11, 14),
    b(11, 13),
    b(11, 12),
    b(11, 11),
    b(11, 10),
    b(11, 9),
    b(11, 8),
    b(11, 7),
    b(12, 7),
    b(13, 7),
    b(13, 8),
    b(13, 9),
    b(13, 10),
    b(13, 11),
    b(14, 11),
    b(15, 11),
    b(16, 11),
    b(17, 11),
    b(18, 11),
];

const TO_BOMBS: &[Cell] = &[
    b(18, 11),
    b(17, 11),
    b(16, 11),
    b(15, 11),
    b(14, 11),
    b(13, 11),
    b(13, 10),
    b(13, 9),
    b(13, 8),
    b(13, 7),
    b(14, 7),
    b(15, 7),
    b(16, 7),
    b(17, 7),
    b(18, 7),
    b(19, 7),
    b(20, 7),
    b(21, 7),
    b(22, 7),
    b(23, 7),
    b(24, 7),
    b(25, 7),
    b(26, 7),
    b(27, 7),
    b(28, 7),
    b(29, 7),
    b(30, 7),
    b(30, 6),
    b(30, 5),
    b(29, 5),
    b(28, 5),
    b(27, 5),
    b(26, 5),
    b(25, 5),
];

const TO_KRAID_DOOR: &[Cell] = &[
    b(25, 5),
    b(26, 5),
    b(27, 5),
    b(28, 5),
    b(29, 5),
    b(30, 5),
    b(30, 6),
    b(30, 7),
    b(29, 7),
    b(28, 7),
    b(27, 7),
    b(26, 7),
    b(25, 7),
    b(24, 7),
    b(23, 7),
    b(22, 7),
    b(21, 7),
    b(20, 7),
    b(19, 7),
    b(18, 7),
    b(17, 7),
    b(16, 7),
    b(15, 7),
    b(14, 7),
    b(13, 7),
    b(12, 7),
    b(11, 7),
    b(11, 8),
    b(11, 9),
    b(11, 10),
    b(11, 11),
    b(11, 12),
    b(11, 13),
    b(11, 14),
    b(10, 14),
    b(9, 14),
    b(8, 14),
    b(7, 14),
    b(6, 14),
    b(6, 15),
    b(6, 16),
    b(6, 17),
    b(6, 18),
    b(7, 18),
    b(7, 19),
    k(7, 19),
    k(7, 20),
    k(7, 21),
    k(7, 22),
    k(7, 23),
    k(7, 24),
    k(7, 25),
    k(7, 26),
    k(8, 26),
    k(9, 26),
    k(9, 27),
    k(10, 27),
    k(11, 27),
    k(11, 28),
    k(10, 28),
    k(9, 28),
    k(9, 29),
];

const TO_NORFAIR: &[Cell] = &[
    k(9, 29),
    k(10, 29),
    k(11, 29),
    k(12, 29),
    k(12, 28),
    k(12, 27),
    k(12, 26),
    k(12, 25),
    k(12, 24),
    k(11, 24),
    k(10, 24),
    k(9, 24),
    k(8, 24),
    k(7, 24),
    k(7, 23),
    k(7, 22),
    k(7, 21),
    k(7, 20),
    k(7, 19),
    b(7, 19),
    b(7, 18),
    b(6, 18),
    b(6, 17),
    b(6, 16),
    b(6, 15),
    b(6, 14),
    b(7, 14),
    b(8, 14),
    b(9, 14),
    b(10, 14),
    b(11, 14),
    b(11, 13),
    b(11, 12),
    b(11, 11),
    b(11, 10),
    b(11, 9),
    b(11, 8),
    b(11, 7),
    b(12, 7),
    b(13, 7),
    b(13, 8),
    b(13, 9),
    b(13, 10),
    b(13, 11),
    b(14, 11),
    b(15, 11),
    b(16, 11),
    b(17, 11),
    b(18, 11),
    b(19, 11),
    b(20, 11),
    b(21, 11),
    b(22, 11),
    b(22, 12),
    n(22, 12),
    n(22, 13),
];

const TO_RIDLEY_ELEVATOR: &[Cell] = &[
    n(22, 13),
    n(22, 14),
    n(23, 14),
    n(24, 14),
    n(25, 14),
    n(26, 14),
    n(27, 14),
    n(28, 14),
    n(29, 14),
    n(30, 14),
    n(30, 15),
    n(30, 16),
    n(30, 17),
    n(30, 18),
    n(30, 19),
    n(30, 20),
    n(30, 21),
    n(29, 21),
    n(28, 21),
    n(27, 21),
    n(26, 21),
    n(25, 21),
    n(24, 21),
    n(24, 22),
    n(25, 22),
    n(25, 23),
    r(25, 23),
    r(25, 24),
];

const TO_RIDLEY_DOOR: &[Cell] = &[
    r(25, 24),
    r(25, 25),
    r(24, 25),
    r(23, 25),
    r(22, 25),
    r(21, 25),
    r(20, 25),
    r(19, 25),
    r(18, 25),
    r(17, 25),
    r(17, 26),
    r(17, 27),
    r(18, 27),
    r(19, 27),
    r(20, 27),
    r(21, 27),
    r(22, 27),
    r(22, 28),
    r(22, 29),
    r(21, 29),
    r(20, 29),
    r(19, 29),
    r(18, 29),
];

const RIDLEY_EXIT: &[Cell] = &[
    r(18, 29),
    r(19, 29),
    r(20, 29),
    r(21, 29),
    r(22, 29),
    r(23, 29),
    r(24, 29),
    r(25, 29),
    r(26, 29),
    r(27, 29),
    r(28, 29),
    r(29, 29),
    r(30, 29),
    r(30, 28),
    r(30, 27),
    r(30, 26),
    r(30, 25),
    r(29, 25),
    r(28, 25),
    r(27, 25),
    r(26, 25),
    r(25, 25),
    r(25, 24),
    r(25, 23),
    n(25, 23),
    n(25, 22),
];

const NORFAIR_RETURN: &[Cell] = &[
    n(25, 22),
    n(24, 22),
    n(24, 21),
    n(25, 21),
    n(26, 21),
    n(27, 21),
    n(28, 21),
    n(29, 21),
    n(30, 21),
    n(30, 20),
    n(30, 19),
    n(30, 18),
    n(30, 17),
    n(30, 16),
    n(30, 15),
    n(30, 14),
    n(29, 14),
    n(28, 14),
    n(27, 14),
    n(26, 14),
    n(25, 14),
    n(24, 14),
    n(23, 14),
    n(22, 14),
    n(22, 13),
    n(22, 12),
    b(22, 12),
    b(22, 11),
];

const TO_TOURIAN: &[Cell] = &[
    b(22, 11),
    b(21, 11),
    b(20, 11),
    b(19, 11),
    b(18, 11),
    b(17, 11),
    b(16, 11),
    b(15, 11),
    b(14, 11),
    b(13, 11),
    b(13, 10),
    b(13, 9),
    b(13, 8),
    b(13, 7),
    b(12, 7),
    b(11, 7),
    b(11, 6),
    b(11, 5),
    b(11, 4),
    b(11, 3),
    b(11, 2),
    b(10, 2),
    b(9, 2),
    b(8, 2),
    b(7, 2),
    b(6, 2),
    b(5, 2),
    b(4, 2),
    b(3, 2),
    b(3, 3),
    t(3, 3),
    t(3, 4),
];

const TO_ZEBETITES: &[Cell] = &[
    t(3, 4),
    t(3, 5),
    t(3, 6),
    t(3, 7),
    t(4, 7),
    t(5, 7),
    t(6, 7),
    t(7, 7),
    t(8, 7),
    t(9, 7),
    t(10, 7),
    t(10, 8),
    t(10, 9),
    t(10, 10),
    t(10, 11),
    t(9, 11),
    t(8, 11),
    t(7, 11),
    t(6, 11),
    t(5, 11),
    t(4, 11),
];

const TO_MOTHER_BRAIN: &[Cell] = &[t(4, 11), t(3, 11), t(2, 11)];

const ESCAPE: &[Cell] = &[
    t(2, 11),
    t(1, 11),
    t(1, 10),
    t(1, 9),
    t(1, 8),
    t(1, 7),
    t(1, 6),
    t(1, 5),
    t(1, 4),
    t(1, 3),
];

const BEFORE_BRINSTAR_ROUTE: Done = Done::Energy(BRINSTAR_TOUCH * TOUCHES_BEFORE_BRINSTAR_ROUTE);
const IN_RIDLEY_HIDEOUT: Done = Done::Energy(RIDLEY_HIDEOUT_TOUCH * TOUCHES_BEFORE_RIDLEY);
const BEFORE_NORFAIR_ROUTE: Done = Done::Energy(NORFAIR_TOUCH * TOUCHES_BEFORE_NORFAIR_ROUTE);

pub const PLAN: &[Stage] = &[
    Stage {
        name: "maru_mari",
        done: Done::Equipment(MARU_MARI),
        objective: Objective::Route(TO_MARU_MARI),
    },
    Stage {
        name: "energy_before_missiles",
        done: BEFORE_BRINSTAR_ROUTE,
        objective: Objective::Energy,
    },
    Stage {
        name: "missiles",
        done: Done::MissileCapacity,
        objective: Objective::Route(TO_MISSILES),
    },
    Stage {
        name: "energy_before_bombs",
        done: BEFORE_BRINSTAR_ROUTE,
        objective: Objective::Energy,
    },
    Stage {
        name: "bombs",
        done: Done::Equipment(BOMBS),
        objective: Objective::Route(TO_BOMBS),
    },
    Stage {
        name: "energy_before_kraid_door",
        done: BEFORE_BRINSTAR_ROUTE,
        objective: Objective::Energy,
    },
    Stage {
        name: "kraid_door",
        done: Done::RouteEnd,
        objective: Objective::Route(TO_KRAID_DOOR),
    },
    Stage {
        name: "energy_before_kraid",
        done: Done::Energy(KRAID_HIDEOUT_TOUCH * TOUCHES_BEFORE_KRAID),
        objective: Objective::Energy,
    },
    Stage {
        name: "kraid",
        done: Done::Kraid,
        objective: Objective::BossDamage,
    },
    Stage {
        name: "energy_after_kraid",
        done: Done::Energy(KRAID_HIDEOUT_TOUCH * TOUCHES_LEAVING_KRAID),
        objective: Objective::Energy,
    },
    Stage {
        name: "norfair_elevator",
        done: Done::RouteEnd,
        objective: Objective::Route(TO_NORFAIR),
    },
    Stage {
        name: "energy_before_ridley_elevator",
        done: BEFORE_NORFAIR_ROUTE,
        objective: Objective::Energy,
    },
    Stage {
        name: "ridley_elevator",
        done: Done::RouteEnd,
        objective: Objective::Route(TO_RIDLEY_ELEVATOR),
    },
    Stage {
        name: "energy_before_ridley_door",
        done: IN_RIDLEY_HIDEOUT,
        objective: Objective::Energy,
    },
    Stage {
        name: "ridley_door",
        done: Done::RouteEnd,
        objective: Objective::Route(TO_RIDLEY_DOOR),
    },
    Stage {
        name: "energy_before_ridley",
        done: IN_RIDLEY_HIDEOUT,
        objective: Objective::Energy,
    },
    Stage {
        name: "ridley",
        done: Done::Ridley,
        objective: Objective::BossDamage,
    },
    Stage {
        name: "energy_after_ridley",
        done: IN_RIDLEY_HIDEOUT,
        objective: Objective::Energy,
    },
    Stage {
        name: "ridley_exit",
        done: Done::RouteEnd,
        objective: Objective::Route(RIDLEY_EXIT),
    },
    Stage {
        name: "energy_before_norfair_return",
        done: BEFORE_NORFAIR_ROUTE,
        objective: Objective::Energy,
    },
    Stage {
        name: "norfair_return",
        done: Done::RouteEnd,
        objective: Objective::Route(NORFAIR_RETURN),
    },
    Stage {
        name: "energy_before_tourian",
        done: Done::Energy(BRINSTAR_TOUCH * TOUCHES_BEFORE_TOURIAN),
        objective: Objective::Energy,
    },
    Stage {
        name: "missiles_before_tourian",
        done: Done::Missiles(MISSILES_BEFORE_TOURIAN),
        objective: Objective::Missiles,
    },
    Stage {
        name: "tourian_elevator",
        done: Done::RouteEnd,
        objective: Objective::Route(TO_TOURIAN),
    },
    Stage {
        name: "zebetite_door",
        done: Done::RouteEnd,
        objective: Objective::Route(TO_ZEBETITES),
    },
    Stage {
        name: "mother_brain_room",
        done: Done::RouteEnd,
        objective: Objective::Route(TO_MOTHER_BRAIN),
    },
    Stage {
        name: "mother_brain",
        done: Done::MotherBrain,
        objective: Objective::BossDamage,
    },
    Stage {
        name: "escape",
        done: Done::Ending,
        objective: Objective::Route(ESCAPE),
    },
    Stage {
        name: "after_ending",
        done: Done::Never,
        objective: Objective::None,
    },
];

pub struct PlanState {
    pub state: MetroidMechanicalState,
    pub boss_damage: u8,
}

fn route_index(route: &[Cell], state: MetroidMechanicalState) -> Option<u16> {
    route
        .iter()
        .position(|cell| *cell == (state.area, state.map_x, state.map_y))
        .and_then(|index| u16::try_from(index).ok())
}

fn whole_energy(health: u16) -> u16 {
    health / ENERGY_UNIT
}

fn energy_reached(state: MetroidMechanicalState, target: u16) -> bool {
    whole_energy(state.health) >= target
}

impl plan::Stage for Stage {
    type State = PlanState;

    fn name(self) -> &'static str {
        self.name
    }

    fn objective(self, state: &PlanState, carried: Option<u16>) -> u16 {
        match self.objective {
            Objective::Route(route) => route_index(route, state.state).or(carried).unwrap_or(0),
            Objective::Energy => whole_energy(state.state.health),
            Objective::Missiles => u16::from(state.state.missiles),
            Objective::BossDamage => u16::from(state.boss_damage),
            Objective::None => 0,
        }
    }

    fn done(self, state: &PlanState, objective: u16) -> bool {
        let state = state.state;
        match self.done {
            Done::Equipment(bit) => state.equipment & bit != 0,
            Done::MissileCapacity => state.missile_capacity > 0,
            Done::RouteEnd => match self.objective {
                Objective::Route(route) => usize::from(objective) + 1 >= route.len(),
                _ => false,
            },
            Done::Energy(target) => energy_reached(state, target),
            Done::Missiles(target) => state.missiles >= target,
            Done::Kraid => state.kraid_defeated,
            Done::Ridley => state.bosses > u8::from(state.kraid_defeated),
            Done::MotherBrain => state.mother_brain_defeated || state.ending,
            Done::Ending => state.ending,
            Done::Never => false,
        }
    }

    fn latches(self) -> bool {
        matches!(
            self.done,
            Done::RouteEnd | Done::Energy(_) | Done::Missiles(_)
        )
    }
}

#[must_use]
pub fn locate(
    state: MetroidMechanicalState,
    boss_damage: u8,
    parent: Option<StageProgress>,
) -> StageProgress {
    plan::locate(PLAN, &PlanState { state, boss_damage }, parent)
}

#[must_use]
pub fn stage_name(stage: u8) -> &'static str {
    plan::stage_name(PLAN, stage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(area: u8, map_x: u8, map_y: u8) -> MetroidMechanicalState {
        MetroidMechanicalState {
            area,
            map_x,
            map_y,
            health: 300,
            ..MetroidMechanicalState::default()
        }
    }

    #[test]
    fn power_on_is_the_maru_mari_stage_at_its_first_cell() {
        assert_eq!(locate(at(BRINSTAR, 3, 14), 0, None), (0, 0));
        assert_eq!(locate(at(BRINSTAR, 2, 14), 0, Some((0, 0))), (0, 1));
    }

    #[test]
    fn the_maru_mari_leads_to_the_energy_stage_before_the_missiles_route() {
        let mut state = at(BRINSTAR, 2, 14);
        state.equipment = MARU_MARI;
        assert_eq!(locate(state, 0, Some((0, 1))), (1, 30));
        state.health = 640;
        assert_eq!(locate(state, 0, Some((1, 63))), (2, 0));
    }

    #[test]
    fn the_route_objective_is_the_current_cell_and_carried_off_the_route() {
        let mut state = at(BRINSTAR, 11, 9);
        state.equipment = MARU_MARI;
        state.health = 640;
        assert_eq!(locate(state, 0, Some((2, 0))), (2, 14));
        let side = MetroidMechanicalState {
            map_x: 13,
            map_y: 3,
            ..state
        };
        assert_eq!(locate(side, 0, Some((2, 14))), (2, 14));
        let behind = MetroidMechanicalState {
            map_x: 5,
            map_y: 14,
            ..state
        };
        assert_eq!(locate(behind, 0, Some((2, 14))), (2, 3));
    }

    #[test]
    fn energy_lost_on_a_route_keeps_the_route_stage() {
        let mut state = at(BRINSTAR, 11, 12);
        state.equipment = MARU_MARI;
        assert_eq!(locate(state, 0, Some((2, 10))), (2, 11));
    }

    #[test]
    fn an_item_moves_to_the_next_energy_stage_or_route() {
        let mut state = at(BRINSTAR, 18, 11);
        state.equipment = MARU_MARI;
        state.missile_capacity = 5;
        assert_eq!(locate(state, 0, Some((2, 27))), (3, 30));
        state.health = 640;
        assert_eq!(locate(state, 0, Some((2, 27))), (4, 0));
    }

    #[test]
    fn items_taken_out_of_order_skip_their_stages() {
        let mut state = at(BRINSTAR, 25, 5);
        state.equipment = MARU_MARI | BOMBS;
        state.health = 640;
        assert_eq!(locate(state, 0, Some((2, 3))), (2, 3));
        state.missile_capacity = 5;
        assert_eq!(locate(state, 0, Some((2, 3))), (6, 0));
    }

    #[test]
    fn reaching_the_kraid_door_latches_and_starts_the_energy_stage() {
        let mut state = at(KRAID, 9, 29);
        state.equipment = MARU_MARI | BOMBS;
        state.missile_capacity = 5;
        assert_eq!(locate(state, 0, Some((6, 55))), (7, 30));
        let away = MetroidMechanicalState {
            map_y: 27,
            health: 450,
            ..state
        };
        assert_eq!(locate(away, 0, Some((7, 30))), (7, 45));
    }

    #[test]
    fn the_energy_target_latches_and_a_hit_afterwards_stays_in_the_boss_stage() {
        let mut state = at(KRAID, 9, 29);
        state.equipment = MARU_MARI | BOMBS;
        state.missile_capacity = 5;
        state.energy_tanks = 1;
        state.health = 805;
        assert_eq!(locate(state, 0, Some((7, 75))), (8, 0));
        let hurt = MetroidMechanicalState {
            map_x: 8,
            health: 900,
            ..state
        };
        assert_eq!(locate(hurt, 3, Some((8, 0))), (8, 3));
        let ridley_only = MetroidMechanicalState { bosses: 1, ..hurt };
        assert_eq!(locate(ridley_only, 3, Some((8, 3))), (8, 3));
        let won = MetroidMechanicalState {
            bosses: 1,
            kraid_defeated: true,
            ..hurt
        };
        assert_eq!(locate(won, 0, Some((8, 3))), (9, 90));
    }

    #[test]
    fn energy_short_of_the_target_keeps_the_energy_stage() {
        let mut state = at(KRAID, 9, 29);
        state.equipment = MARU_MARI | BOMBS;
        state.missile_capacity = 5;
        state.energy_tanks = 1;
        state.health = 795;
        assert_eq!(locate(state, 0, Some((7, 75))), (7, 79));
    }

    #[test]
    fn the_targets_are_eight_brinstar_touches_and_four_hideout_touches() {
        assert_eq!(BRINSTAR_TOUCH * TOUCHES_BEFORE_BRINSTAR_ROUTE, 64);
        assert_eq!(KRAID_HIDEOUT_TOUCH * TOUCHES_BEFORE_KRAID, 80);
        let mut state = at(KRAID, 9, 29);
        state.equipment = MARU_MARI | BOMBS;
        state.missile_capacity = 5;
        state.health = 790;
        assert_eq!(locate(state, 0, Some((7, 75))), (7, 79));
        state.health = 999;
        assert_eq!(locate(state, 0, Some((7, 79))), (8, 0));
    }

    fn after_kraid(area: u8, map_x: u8, map_y: u8) -> MetroidMechanicalState {
        MetroidMechanicalState {
            equipment: MARU_MARI | BOMBS,
            missile_capacity: 80,
            energy_tanks: 1,
            bosses: 1,
            kraid_defeated: true,
            ..at(area, map_x, map_y)
        }
    }

    #[test]
    fn the_targets_after_kraid_are_hideout_norfair_and_ridley_touches() {
        assert_eq!(KRAID_HIDEOUT_TOUCH * TOUCHES_LEAVING_KRAID, 120);
        assert_eq!(NORFAIR_TOUCH * TOUCHES_BEFORE_NORFAIR_ROUTE, 80);
        assert_eq!(RIDLEY_HIDEOUT_TOUCH * TOUCHES_BEFORE_RIDLEY, 96);
    }

    #[test]
    fn energy_after_kraid_leads_to_the_route_out_of_the_hideout() {
        let mut state = after_kraid(KRAID, 8, 29);
        state.health = 1190;
        assert_eq!(locate(state, 0, Some((8, 12))), (9, 119));
        state.health = 1999;
        assert_eq!(locate(state, 0, Some((9, 119))), (10, 0));
        let climbing = MetroidMechanicalState {
            map_x: 7,
            map_y: 22,
            health: 1500,
            ..state
        };
        assert_eq!(locate(climbing, 0, Some((10, 9))), (10, 15));
    }

    #[test]
    fn the_norfair_arrival_latches_and_starts_the_norfair_energy_stage() {
        let mut state = after_kraid(NORFAIR, 22, 13);
        state.health = 700;
        assert_eq!(locate(state, 0, Some((10, 52))), (11, 70));
        state.health = 800;
        assert_eq!(locate(state, 0, Some((11, 70))), (12, 0));
        let bombed = MetroidMechanicalState {
            map_x: 30,
            map_y: 16,
            health: 500,
            ..state
        };
        assert_eq!(locate(bombed, 0, Some((12, 10))), (12, 11));
    }

    #[test]
    fn the_ridley_elevator_and_door_routes_end_in_ridley_energy_stages() {
        let mut state = after_kraid(RIDLEY, 25, 24);
        state.health = 900;
        assert_eq!(locate(state, 0, Some((12, 24))), (13, 90));
        state.health = 960;
        assert_eq!(locate(state, 0, Some((13, 90))), (14, 0));
        let door = MetroidMechanicalState {
            map_x: 18,
            map_y: 29,
            health: 500,
            ..state
        };
        assert_eq!(locate(door, 0, Some((14, 21))), (15, 50));
    }

    #[test]
    fn ridley_damage_climbs_the_boss_stage_and_his_defeat_starts_the_way_back() {
        let mut state = after_kraid(RIDLEY, 17, 29);
        state.health = 960;
        assert_eq!(locate(state, 0, Some((15, 96))), (16, 0));
        assert_eq!(locate(state, 5, Some((16, 0))), (16, 5));
        state.bosses = 2;
        assert_eq!(locate(state, 0, Some((16, 5))), (18, 0));
        state.health = 400;
        assert_eq!(locate(state, 0, Some((16, 5))), (17, 40));
        assert_eq!(stage_name(17), "energy_after_ridley");
    }

    fn after_ridley(area: u8, map_x: u8, map_y: u8) -> MetroidMechanicalState {
        MetroidMechanicalState {
            bosses: 2,
            missile_capacity: 155,
            ..after_kraid(area, map_x, map_y)
        }
    }

    #[test]
    fn the_way_back_climbs_routes_through_ridley_norfair_and_brinstar() {
        let mut state = after_ridley(RIDLEY, 30, 27);
        state.health = 500;
        assert_eq!(locate(state, 0, Some((18, 13))), (18, 14));
        let norfair = MetroidMechanicalState {
            area: NORFAIR,
            map_x: 30,
            map_y: 18,
            ..state
        };
        assert_eq!(locate(norfair, 0, Some((19, 50))), (19, 50));
        let healed = MetroidMechanicalState {
            health: 800,
            ..norfair
        };
        assert_eq!(locate(healed, 0, Some((19, 79))), (20, 11));
        let brinstar = MetroidMechanicalState {
            area: BRINSTAR,
            map_x: 22,
            map_y: 11,
            ..healed
        };
        assert_eq!(locate(brinstar, 0, Some((20, 26))), (21, 80));
    }

    #[test]
    fn the_missile_target_latches_before_the_tourian_route() {
        let mut state = after_ridley(BRINSTAR, 13, 9);
        state.health = 960;
        state.missiles = 40;
        assert_eq!(locate(state, 0, Some((21, 96))), (22, 40));
        state.missiles = 80;
        assert_eq!(locate(state, 0, Some((22, 79))), (23, 11));
        state.missiles = 10;
        state.health = 300;
        assert_eq!(locate(state, 0, Some((23, 11))), (23, 11));
    }

    #[test]
    fn mother_brain_damage_leads_to_the_escape_and_the_ending_ends_the_plan() {
        let mut state = after_ridley(TOURIAN, 2, 11);
        state.health = 300;
        assert_eq!(locate(state, 0, Some((25, 1))), (26, 0));
        assert_eq!(locate(state, 7, Some((26, 0))), (26, 7));
        state.mother_brain_defeated = true;
        assert_eq!(locate(state, 0, Some((26, 7))), (27, 0));
        let climbing = MetroidMechanicalState {
            map_x: 1,
            map_y: 6,
            ..state
        };
        assert_eq!(locate(climbing, 0, Some((27, 5))), (27, 6));
        let ended = MetroidMechanicalState {
            ending: true,
            mother_brain_defeated: false,
            ..climbing
        };
        assert_eq!(locate(ended, 0, Some((27, 9))), (28, 0));
        assert_eq!(stage_name(28), "after_ending");
    }

    #[test]
    fn every_route_starts_where_the_previous_stage_ends() {
        let routes: Vec<&[Cell]> = PLAN
            .iter()
            .filter_map(|stage| match stage.objective {
                Objective::Route(route) => Some(route),
                _ => None,
            })
            .collect();
        for pair in routes.windows(2) {
            assert_eq!(pair[0].last(), pair[1].first());
        }
        for route in routes {
            for (index, cell) in route.iter().enumerate() {
                assert!(!route[..index].contains(cell));
            }
        }
    }
}
