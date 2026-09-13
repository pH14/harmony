// SPDX-License-Identifier: AGPL-3.0-or-later

pub const REG_TICKS: u32 = 1;
pub const REG_ALIVE: u32 = 2;
pub const REG_HOOKS_STARTED: u32 = 3;
pub const REG_HOOKS_FINISHED: u32 = 4;
pub const REG_SOMETIMES: u32 = 5;
pub const REG_UNEXPECTED_DEATHS: u32 = 6;
pub const REG_RESTARTS: u32 = 7;
pub const REG_PARKED: u32 = 8;
pub const REG_EVENT_KILL_FIRES: u32 = 9;
pub const REG_EVENT_KILL_SITE: u32 = 10;
pub const REG_EVENT_PARK_FIRES: u32 = 11;
pub const REG_WORKLOAD_STARTED: u32 = 12;
pub const REG_WORKLOAD_FINISHED: u32 = 13;
pub const REG_CHECKS_STARTED: u32 = 14;
pub const REG_CHECKS_FINISHED: u32 = 15;
pub const REG_INFRASTRUCTURE_ERROR: u32 = 16;
pub const REG_EVENT_READY: u32 = 17;
pub const REG_DISTURBANCE_GENERATION: u32 = 18;
pub const REG_CHECK_ENABLED: u32 = 19;
pub const REG_COMPLETED_CHECK_RUN: u32 = 20;
pub const REG_COMPLETED_CHECK_START_GENERATION: u32 = 21;
pub const REG_COMPLETED_CHECK_END_GENERATION: u32 = 22;
pub const REG_COMPLETED_CHECK_POINTS: u32 = 23;
pub const REG_PENDING_FAULTS: u32 = 24;

pub const SOMETIMES_BITMAP_IDS: u32 = 48;
pub const CHECK_POINT_IDS: u32 = SOMETIMES_BITMAP_IDS;

#[must_use]
pub fn point_bit(id: u32) -> Option<u64> {
    (id < CHECK_POINT_IDS).then(|| 1_u64 << id)
}

#[must_use]
pub fn sometimes_bit(id: u32) -> Option<u64> {
    point_bit(id)
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
    pub event_kill_fires: u64,
    pub event_kill_site: u64,
    pub event_park_fires: u64,
    pub workload_started: u64,
    pub workload_finished: u64,
    pub checks_started: u64,
    pub checks_finished: u64,
    pub infrastructure_error: u64,
    pub event_ready: u64,
    pub disturbance_generation: u64,
    pub check_enabled: u64,
    pub completed_check_run: u64,
    pub completed_check_start_generation: u64,
    pub completed_check_end_generation: u64,
    pub completed_check_points: u64,
    pub pending_faults: u64,
}

impl RegisterSnapshot {
    #[must_use]
    pub fn pairs(&self) -> [(u32, u64); 24] {
        [
            (REG_TICKS, self.ticks),
            (REG_ALIVE, self.alive),
            (REG_HOOKS_STARTED, self.hooks_started),
            (REG_HOOKS_FINISHED, self.hooks_finished),
            (REG_SOMETIMES, self.sometimes),
            (REG_UNEXPECTED_DEATHS, self.unexpected_deaths),
            (REG_RESTARTS, self.restarts),
            (REG_PARKED, self.parked),
            (REG_EVENT_KILL_FIRES, self.event_kill_fires),
            (REG_EVENT_KILL_SITE, self.event_kill_site),
            (REG_EVENT_PARK_FIRES, self.event_park_fires),
            (REG_WORKLOAD_STARTED, self.workload_started),
            (REG_WORKLOAD_FINISHED, self.workload_finished),
            (REG_CHECKS_STARTED, self.checks_started),
            (REG_CHECKS_FINISHED, self.checks_finished),
            (REG_INFRASTRUCTURE_ERROR, self.infrastructure_error),
            (REG_EVENT_READY, self.event_ready),
            (REG_DISTURBANCE_GENERATION, self.disturbance_generation),
            (REG_CHECK_ENABLED, self.check_enabled),
            (REG_COMPLETED_CHECK_RUN, self.completed_check_run),
            (
                REG_COMPLETED_CHECK_START_GENERATION,
                self.completed_check_start_generation,
            ),
            (
                REG_COMPLETED_CHECK_END_GENERATION,
                self.completed_check_end_generation,
            ),
            (REG_COMPLETED_CHECK_POINTS, self.completed_check_points),
            (REG_PENDING_FAULTS, self.pending_faults),
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
                    .filter(|((reg, value), (_, was))| {
                        *reg == REG_TICKS || *reg == REG_PENDING_FAULTS || value != was
                    })
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
                (REG_EVENT_KILL_FIRES, 0),
                (REG_EVENT_KILL_SITE, 0),
                (REG_EVENT_PARK_FIRES, 0),
                (REG_WORKLOAD_STARTED, 0),
                (REG_WORKLOAD_FINISHED, 0),
                (REG_CHECKS_STARTED, 0),
                (REG_CHECKS_FINISHED, 0),
                (REG_INFRASTRUCTURE_ERROR, 0),
                (REG_EVENT_READY, 0),
                (REG_DISTURBANCE_GENERATION, 0),
                (REG_CHECK_ENABLED, 0),
                (REG_COMPLETED_CHECK_RUN, 0),
                (REG_COMPLETED_CHECK_START_GENERATION, 0),
                (REG_COMPLETED_CHECK_END_GENERATION, 0),
                (REG_COMPLETED_CHECK_POINTS, 0),
                (REG_PENDING_FAULTS, 0),
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
        assert_eq!(
            regs.updates(snapshot),
            [(REG_TICKS, 2), (REG_PENDING_FAULTS, 0)]
        );

        snapshot.ticks = 3;
        snapshot.alive = 0;
        snapshot.unexpected_deaths = 1;
        assert_eq!(
            regs.updates(snapshot),
            [
                (REG_TICKS, 3),
                (REG_ALIVE, 0),
                (REG_UNEXPECTED_DEATHS, 1),
                (REG_PENDING_FAULTS, 0)
            ]
        );

        snapshot.ticks = 4;
        assert_eq!(
            regs.updates(snapshot),
            [(REG_TICKS, 4), (REG_PENDING_FAULTS, 0)]
        );
    }

    #[test]
    fn pending_faults_are_republished_after_an_invalid_fence() {
        let mut regs = Registers::new();
        let mut snapshot = RegisterSnapshot {
            ticks: 1,
            pending_faults: 2,
            ..RegisterSnapshot::default()
        };
        regs.updates(snapshot);

        snapshot.ticks = 2;
        assert_eq!(
            regs.updates(snapshot),
            [(REG_TICKS, 2), (REG_PENDING_FAULTS, 2)]
        );
    }

    #[test]
    fn the_sometimes_bitmap_is_forty_eight_ids_wide() {
        assert_eq!(sometimes_bit(0), Some(1));
        assert_eq!(sometimes_bit(47), Some(1 << 47));
        assert_eq!(sometimes_bit(48), None);
        assert_eq!(sometimes_bit(u32::MAX), None);
    }
}
