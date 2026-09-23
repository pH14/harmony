// SPDX-License-Identifier: AGPL-3.0-or-later

pub use process_proto::registers::SUPERVISOR_REGISTER_BASE;
pub use process_proto::registers::{
    ALIVE as REG_ALIVE, CHECK_ENABLED as REG_CHECK_ENABLED, CHECKS_FINISHED as REG_CHECKS_FINISHED,
    CHECKS_STARTED as REG_CHECKS_STARTED,
    COMPLETED_CHECK_END_GENERATION as REG_COMPLETED_CHECK_END_GENERATION,
    COMPLETED_CHECK_POINTS as REG_COMPLETED_CHECK_POINTS,
    COMPLETED_CHECK_RUN as REG_COMPLETED_CHECK_RUN,
    COMPLETED_CHECK_START_GENERATION as REG_COMPLETED_CHECK_START_GENERATION,
    DISTURBANCE_GENERATION as REG_DISTURBANCE_GENERATION, EVENT_KILL_FIRES as REG_EVENT_KILL_FIRES,
    EVENT_KILL_SITE as REG_EVENT_KILL_SITE, EVENT_PARK_FIRES as REG_EVENT_PARK_FIRES,
    EVENT_READY as REG_EVENT_READY, HOOKS_FINISHED as REG_HOOKS_FINISHED,
    HOOKS_STARTED as REG_HOOKS_STARTED, INFRASTRUCTURE_ERROR as REG_INFRASTRUCTURE_ERROR,
    PENDING_FAULTS as REG_PENDING_FAULTS, RESTARTS as REG_RESTARTS, SOMETIMES as REG_SOMETIMES,
    TICKS as REG_TICKS, UNEXPECTED_DEATHS as REG_UNEXPECTED_DEATHS,
    WORKLOAD_FINISHED as REG_WORKLOAD_FINISHED, WORKLOAD_STARTED as REG_WORKLOAD_STARTED,
};

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
    pub fn pairs(&self) -> [(u32, u64); 23] {
        [
            (REG_TICKS, self.ticks),
            (REG_ALIVE, self.alive),
            (REG_HOOKS_STARTED, self.hooks_started),
            (REG_HOOKS_FINISHED, self.hooks_finished),
            (REG_SOMETIMES, self.sometimes),
            (REG_UNEXPECTED_DEATHS, self.unexpected_deaths),
            (REG_RESTARTS, self.restarts),
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
    fn supervisor_registers_occupy_a_high_reserved_range() {
        assert_eq!(SUPERVISOR_REGISTER_BASE, 0x00ff_f000);
        assert_eq!(REG_EVENT_KILL_FIRES, SUPERVISOR_REGISTER_BASE + 7);
    }

    #[test]
    fn the_sometimes_bitmap_is_forty_eight_ids_wide() {
        assert_eq!(sometimes_bit(0), Some(1));
        assert_eq!(sometimes_bit(47), Some(1 << 47));
        assert_eq!(sometimes_bit(48), None);
        assert_eq!(sometimes_bit(u32::MAX), None);
    }
}
