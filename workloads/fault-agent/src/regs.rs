// SPDX-License-Identifier: AGPL-3.0-or-later
//! The IJON state registers the agent publishes.
//!
//! | reg | meaning |
//! |---|---|
//! | 1 | completed poll ticks |
//! | 2 | alive bitmap, one bit per node |
//! | 3 | hooks started |
//! | 4 | hooks finished |
//! | 5 | bitmap of the `assert_sometimes` ids a hook reported |
//! | 6 | node exits not expected from Kill or Restart |
//! | 7 | node starts after the initial one |
//! | 8 | threads the guest kernel parked at a place |
//! | 9 | node deaths observed while an EventKill arm was active |
//!
//! The tick register is emitted every tick so the host always has a fresh
//! liveness signal; the others are emitted only when their value changes, which
//! keeps a long idle run from burying the event stream in repeats.

/// Completed poll ticks.
pub const REG_TICKS: u32 = 1;
/// Alive bitmap, one bit per node.
pub const REG_ALIVE: u32 = 2;
/// Hooks started.
pub const REG_HOOKS_STARTED: u32 = 3;
/// Hooks finished.
pub const REG_HOOKS_FINISHED: u32 = 4;
/// Bitmap of reported `assert_sometimes` ids.
pub const REG_SOMETIMES: u32 = 5;
/// Node exits with no fault in force.
pub const REG_UNEXPECTED_DEATHS: u32 = 6;
/// Node starts after the initial one.
pub const REG_RESTARTS: u32 = 7;
/// Threads the guest kernel parked at a place.
pub const REG_PARKED: u32 = 8;
/// Node deaths observed while an EventKill arm was active.
pub const REG_EVENT_KILLS_FIRED: u32 = 9;
/// Exits of the bundle's `workload` process. The agent never restarts it, so a
/// non-zero value means the load stopped and later evidence is weaker.
pub const REG_WORKLOAD_DEATHS: u32 = 10;

/// Agent ticks between the most recent EventKill arm and the death it caused.
/// It says how far past its arm an event coordinate actually reached, which is
/// what tells a coordinate that fired immediately from one that ran on.
pub const REG_EVENT_KILL_AGE_TICKS: u32 = 11;

/// Runs of the bundle's `check` command that finished. It separates a run
/// whose oracle never completed from one whose oracle completed and found
/// nothing, which read the same in the assertion evidence.
pub const REG_CHECKS_FINISHED: u32 = 12;

/// The number of `assert_sometimes` ids [`REG_SOMETIMES`] can hold. A hit at a
/// higher id still reaches the host as an assertion event; it just has no bit.
pub const SOMETIMES_BITMAP_IDS: u32 = 48;

/// The bit `id` occupies in [`REG_SOMETIMES`], or `None` when the id is beyond
/// the bitmap.
#[must_use]
pub fn sometimes_bit(id: u32) -> Option<u64> {
    (id < SOMETIMES_BITMAP_IDS).then(|| 1_u64 << id)
}

/// The register values at one tick.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegisterSnapshot {
    /// [`REG_TICKS`].
    pub ticks: u64,
    /// [`REG_ALIVE`].
    pub alive: u64,
    /// [`REG_HOOKS_STARTED`].
    pub hooks_started: u64,
    /// [`REG_HOOKS_FINISHED`].
    pub hooks_finished: u64,
    /// [`REG_SOMETIMES`].
    pub sometimes: u64,
    /// [`REG_UNEXPECTED_DEATHS`].
    pub unexpected_deaths: u64,
    /// [`REG_RESTARTS`].
    pub restarts: u64,
    /// [`REG_PARKED`].
    pub parked: u64,
    /// [`REG_EVENT_KILLS_FIRED`].
    pub event_kills_fired: u64,
    /// [`REG_WORKLOAD_DEATHS`].
    pub workload_deaths: u64,
    /// Agent ticks the most recent fired EventKill arm survived.
    pub event_kill_age_ticks: u64,
    /// [`REG_CHECKS_FINISHED`].
    pub checks_finished: u64,
}

impl RegisterSnapshot {
    /// The `(register, value)` pairs in register order.
    #[must_use]
    pub fn pairs(&self) -> [(u32, u64); 12] {
        [
            (REG_TICKS, self.ticks),
            (REG_ALIVE, self.alive),
            (REG_HOOKS_STARTED, self.hooks_started),
            (REG_HOOKS_FINISHED, self.hooks_finished),
            (REG_SOMETIMES, self.sometimes),
            (REG_UNEXPECTED_DEATHS, self.unexpected_deaths),
            (REG_RESTARTS, self.restarts),
            (REG_PARKED, self.parked),
            (REG_EVENT_KILLS_FIRED, self.event_kills_fired),
            (REG_WORKLOAD_DEATHS, self.workload_deaths),
            (REG_EVENT_KILL_AGE_TICKS, self.event_kill_age_ticks),
            (REG_CHECKS_FINISHED, self.checks_finished),
        ]
    }
}

/// Tracks what has already been published so a tick emits only what changed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Registers {
    last: Option<RegisterSnapshot>,
}

impl Registers {
    /// A tracker that has published nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The `(register, value)` pairs to emit for `snapshot`: the tick register
    /// always, every other register whose value moved, and on the first call
    /// all of them so the host starts from a complete picture.
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
                (REG_EVENT_KILLS_FIRED, 0),
                (REG_WORKLOAD_DEATHS, 0),
                (REG_EVENT_KILL_AGE_TICKS, 0),
                (REG_CHECKS_FINISHED, 0),
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
    fn event_kill_fired_is_published_as_its_own_monotonic_register() {
        let mut regs = Registers::new();
        regs.updates(RegisterSnapshot::default());
        let updates = regs.updates(RegisterSnapshot {
            ticks: 1,
            event_kills_fired: 1,
            ..RegisterSnapshot::default()
        });
        assert_eq!(updates, [(REG_TICKS, 1), (REG_EVENT_KILLS_FIRED, 1)]);
    }

    #[test]
    fn the_sometimes_bitmap_is_forty_eight_ids_wide() {
        assert_eq!(sometimes_bit(0), Some(1));
        assert_eq!(sometimes_bit(47), Some(1 << 47));
        assert_eq!(sometimes_bit(48), None);
        assert_eq!(sometimes_bit(u32::MAX), None);
    }
}
