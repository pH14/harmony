// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, fmt::Debug, sync::Arc};

use crate::{
    metroid::{
        archive::{KEY_POLICY_IDENTIFIER, MetroidArchiveKey, PREFERENCE_IDENTIFIER, archive_key},
        game::Metroid,
        target::MetroidMechanicalState,
    },
    search::{
        archive::{ArchiveKey, KeyShape},
        assertion::AssertionKey,
    },
    workload::{Game, Lowered},
};

pub trait MetroidKeying: Send + Sync + 'static {
    type Key: ArchiveKey + Debug + Eq + Send + Sync + 'static;

    fn key(&self, state: MetroidMechanicalState, boss_health_seen: u16) -> Self::Key;
    fn shape(&self) -> KeyShape;
    fn key_policy(&self) -> &str;
    fn preference_policy(&self) -> &str;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RouteKeying;

impl MetroidKeying for RouteKeying {
    type Key = MetroidArchiveKey;

    fn key(&self, state: MetroidMechanicalState, boss_health_seen: u16) -> MetroidArchiveKey {
        archive_key(state).with_boss_health_seen(boss_health_seen)
    }

    fn shape(&self) -> KeyShape {
        KeyShape::of::<MetroidArchiveKey>()
    }

    fn key_policy(&self) -> &str {
        KEY_POLICY_IDENTIFIER
    }

    fn preference_policy(&self) -> &str {
        PREFERENCE_IDENTIFIER
    }
}

#[derive(Clone, Debug)]
pub struct StrategyKeying {
    lowered: Arc<Lowered<Metroid>>,
}

impl StrategyKeying {
    pub fn new(strategy: &str) -> Result<Self, Box<dyn Error>> {
        let lowered = Lowered::new(Metroid, strategy)?;
        if !lowered.compiled.is_pure() {
            return Err(
                "a Metroid strategy in the hand campaign has no aggregates or carried stages"
                    .into(),
            );
        }
        Ok(Self {
            lowered: Arc::new(lowered),
        })
    }

    #[must_use]
    pub fn lowered(&self) -> &Lowered<Metroid> {
        &self.lowered
    }
}

impl Default for StrategyKeying {
    fn default() -> Self {
        Self::new(Metroid::STRATEGY).expect("the built-in Metroid strategy compiles")
    }
}

impl MetroidKeying for StrategyKeying {
    type Key = AssertionKey;

    fn key(&self, state: MetroidMechanicalState, _boss_health_seen: u16) -> AssertionKey {
        let (record, _) = self.lowered.record(&state);
        self.lowered.compiled.key(&record).unwrap_or_default()
    }

    fn shape(&self) -> KeyShape {
        self.lowered.compiled.shape()
    }

    fn key_policy(&self) -> &str {
        self.lowered.compiled.policy_identifier()
    }

    fn preference_policy(&self) -> &str {
        self.lowered.compiled.policy_identifier()
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::*;
    use crate::search::rand::RomuDuoJrRand;

    fn draw(rand: &mut RomuDuoJrRand) -> MetroidMechanicalState {
        let mut byte = |range: u64| u8::try_from(rand.next_u64() % range).unwrap();
        MetroidMechanicalState {
            area: 0x10 + byte(5),
            map_x: byte(4),
            map_y: byte(4),
            x: byte(64) * 4,
            y: byte(64) * 4,
            pose: byte(4),
            door: byte(3),
            missiles: byte(4) * 5,
            missile_capacity: byte(3) * 10,
            health: u16::from(byte(8)) * 125,
            equipment: byte(32),
            boss_health: u16::from(byte(3)) * 32,
            mode: 3,
            ..MetroidMechanicalState::default()
        }
    }

    #[test]
    fn strategy_a_keeps_one_state_per_position_and_prefers_missiles_then_health() {
        let keying = StrategyKeying::default();
        assert_eq!(
            keying.shape(),
            KeyShape {
                capacity: 1,
                preferences: 1,
                ties_above: 0
            }
        );
        let mut rand = RomuDuoJrRand::with_seed(0x6d65_7472);
        let states: Vec<MetroidMechanicalState> = (0..400).map(|_| draw(&mut rand)).collect();
        for a in &states {
            let ka = keying.key(*a, 0);
            for b in &states {
                let kb = keying.key(*b, 0);
                assert_eq!(ka.progress(), kb.progress());
                assert_eq!(
                    ka.place().cmp(&kb.place()),
                    (a.area, a.map_x, a.map_y).cmp(&(b.area, b.map_x, b.map_y))
                );
                assert_eq!(
                    ka.identity().cmp(&kb.identity()),
                    (a.x / 16, a.y / 16).cmp(&(b.x / 16, b.y / 16))
                );
                assert_eq!(
                    ka.preference_cmp(0, kb),
                    (a.missiles, a.health).cmp(&(b.missiles, b.health))
                );
            }
        }
    }

    #[test]
    fn strategy_a_ignores_equipment_bosses_and_posture() {
        let keying = StrategyKeying::default();
        let base = MetroidMechanicalState {
            area: 0x10,
            map_x: 3,
            map_y: 14,
            x: 100,
            y: 0xb0,
            health: 300,
            mode: 3,
            ..MetroidMechanicalState::default()
        };
        let changed = MetroidMechanicalState {
            equipment: 0x11,
            boss_health: 64,
            pose: 2,
            door: 1,
            missile_capacity: 25,
            energy_tanks: 2,
            kraid_defeated: true,
            ..base
        };
        assert_eq!(
            keying
                .key(base, 0)
                .preference_cmp(0, keying.key(changed, 0)),
            Ordering::Equal
        );
        assert_eq!(keying.key(base, 0), keying.key(changed, 0));
    }

    #[test]
    fn the_route_keying_is_the_hand_key() {
        let state = MetroidMechanicalState {
            area: 0x10,
            map_x: 2,
            map_y: 14,
            health: 300,
            ..MetroidMechanicalState::default()
        };
        assert_eq!(
            RouteKeying.key(state, 40),
            archive_key(state).with_boss_health_seen(40)
        );
        assert_eq!(RouteKeying.shape(), KeyShape::of::<MetroidArchiveKey>());
        assert_eq!(RouteKeying.key_policy(), KEY_POLICY_IDENTIFIER);
    }
}
