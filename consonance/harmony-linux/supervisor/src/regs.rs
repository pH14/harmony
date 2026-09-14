// SPDX-License-Identifier: AGPL-3.0-or-later

pub const SUPERVISOR_REGISTER_BASE: u32 = 0x00ff_f000;
pub const REG_TICKS: u32 = SUPERVISOR_REGISTER_BASE;
pub const REG_ALIVE: u32 = SUPERVISOR_REGISTER_BASE + 1;
pub const REG_HOOKS_STARTED: u32 = SUPERVISOR_REGISTER_BASE + 2;
pub const REG_HOOKS_FINISHED: u32 = SUPERVISOR_REGISTER_BASE + 3;
pub const REG_SOMETIMES: u32 = SUPERVISOR_REGISTER_BASE + 4;
pub const REG_UNEXPECTED_DEATHS: u32 = SUPERVISOR_REGISTER_BASE + 5;
pub const REG_RESTARTS: u32 = SUPERVISOR_REGISTER_BASE + 6;
pub const REG_PARKED: u32 = SUPERVISOR_REGISTER_BASE + 7;

pub const SOMETIMES_BITMAP_IDS: u32 = 48;

#[must_use]
pub fn sometimes_bit(id: u32) -> Option<u64> {
    (id < SOMETIMES_BITMAP_IDS).then(|| 1_u64 << id)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegisterSnapshot {
    pub ticks: u64,
    pub alive: u64,
    pub hooks_started: u64,
    pub hooks_finished: u64,
    pub sometimes: u64,
    pub unexpected_deaths: u64,
    pub restarts: u64,
    pub parked: u64,
}

impl RegisterSnapshot {
    #[must_use]
    pub fn pairs(&self) -> [(u32, u64); 8] {
        [
            (REG_TICKS, self.ticks),
            (REG_ALIVE, self.alive),
            (REG_HOOKS_STARTED, self.hooks_started),
            (REG_HOOKS_FINISHED, self.hooks_finished),
            (REG_SOMETIMES, self.sometimes),
            (REG_UNEXPECTED_DEATHS, self.unexpected_deaths),
            (REG_RESTARTS, self.restarts),
            (REG_PARKED, self.parked),
        ]
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Registers {
    last: Option<RegisterSnapshot>,
}

impl Registers {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn updates(&mut self, snapshot: RegisterSnapshot) -> Vec<(u32, u64)> {
        let pairs = snapshot.pairs();
        let updates = match self.last {
            None => pairs.to_vec(),
            Some(last) => {
                let previous = last.pairs();
                pairs
                    .into_iter()
                    .zip(previous)
                    .filter(|((reg, value), (_, was))| *reg == REG_TICKS || value != was)
                    .map(|(current, _)| current)
                    .collect()
            }
        };
        self.last = Some(snapshot);
        updates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_tick_publishes_every_register() {
        let mut regs = Registers::new();
        let snapshot = RegisterSnapshot {
            ticks: 1,
            alive: 0b11,
            ..RegisterSnapshot::default()
        };
        assert_eq!(
            regs.updates(snapshot),
            [
                (REG_TICKS, 1),
                (REG_ALIVE, 0b11),
                (REG_HOOKS_STARTED, 0),
                (REG_HOOKS_FINISHED, 0),
                (REG_SOMETIMES, 0),
                (REG_UNEXPECTED_DEATHS, 0),
                (REG_RESTARTS, 0),
                (REG_PARKED, 0),
            ]
        );
    }

    #[test]
    fn later_ticks_publish_the_tick_and_the_changes() {
        let mut regs = Registers::new();
        let mut snapshot = RegisterSnapshot {
            ticks: 1,
            alive: 1,
            ..RegisterSnapshot::default()
        };
        regs.updates(snapshot);

        snapshot.ticks = 2;
        assert_eq!(regs.updates(snapshot), [(REG_TICKS, 2)]);

        snapshot.ticks = 3;
        snapshot.alive = 0;
        snapshot.unexpected_deaths = 1;
        assert_eq!(
            regs.updates(snapshot),
            [(REG_TICKS, 3), (REG_ALIVE, 0), (REG_UNEXPECTED_DEATHS, 1)]
        );

        snapshot.ticks = 4;
        assert_eq!(regs.updates(snapshot), [(REG_TICKS, 4)]);
    }

    #[test]
    fn supervisor_registers_occupy_a_high_reserved_range() {
        assert_eq!(SUPERVISOR_REGISTER_BASE, 0x00ff_f000);
        assert_eq!(REG_PARKED, SUPERVISOR_REGISTER_BASE + 7);
    }

    #[test]
    fn the_sometimes_bitmap_is_forty_eight_ids_wide() {
        assert_eq!(sometimes_bit(0), Some(1));
        assert_eq!(sometimes_bit(47), Some(1 << 47));
        assert_eq!(sometimes_bit(48), None);
        assert_eq!(sometimes_bit(u32::MAX), None);
    }
}
