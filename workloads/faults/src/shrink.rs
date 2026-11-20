// SPDX-License-Identifier: AGPL-3.0-or-later

use std::num::NonZeroU16;

use searcher::search::{cost::ShrinkCost, shrink::ShrinkPolicy};

use crate::{
    campaign::FaultWorkload,
    target::{FaultAction, FaultOperation},
};

#[must_use]
pub fn fault_cost(action: &FaultAction) -> ShrinkCost {
    ShrinkCost::new(u64::from(action.is_fault()), action.ticks())
}

#[must_use]
pub fn simpler_fault_actions(action: &FaultAction) -> Vec<FaultAction> {
    let mut simpler = Vec::with_capacity(2);
    if action.is_fault() {
        simpler.push(action.as_wait());
    }
    if let Some(half) = u16::try_from(action.ticks() / 2)
        .ok()
        .and_then(NonZeroU16::new)
    {
        simpler.push(action.with_ticks(half));
    }
    simpler
}

#[must_use]
pub fn settle_tail(last: Option<&FaultAction>) -> Vec<FaultAction> {
    let quantum = last.map_or(NonZeroU16::MIN, |action| action.coverage_quantum);
    crate::package::REPLAY_SETTLE_TICKS
        .into_iter()
        .filter_map(NonZeroU16::new)
        .map(|ticks| FaultAction::new(FaultOperation::Wait(ticks), quantum))
        .collect()
}

impl ShrinkPolicy for FaultWorkload {}

#[cfg(test)]
mod tests {
    use searcher::search::campaign::InputPolicy;

    use super::*;
    use crate::{
        campaign::{fixture_run, fixture_workload},
        consonance::{DEFAULT_RAM_MIB, FaultConfig, GuestBackend},
        target::FaultOperation,
    };

    fn ticks(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).unwrap()
    }

    fn action(operation: FaultOperation) -> FaultAction {
        FaultAction::new(operation, ticks(8)).with_choice(11)
    }

    #[test]
    fn a_fault_costs_one_fault_and_its_ticks_and_a_wait_only_its_ticks() {
        assert_eq!(
            fault_cost(&action(FaultOperation::Kill(1, ticks(256)))),
            ShrinkCost::new(1, 256)
        );
        assert_eq!(
            fault_cost(&action(FaultOperation::Wait(ticks(64)))),
            ShrinkCost::new(0, 64)
        );
    }

    #[test]
    fn a_fault_becomes_a_wait_of_its_length_first_then_half_as_long() {
        let workload = fixture_workload(
            b"kernel",
            b"initramfs",
            &FaultConfig {
                knobs: Vec::new(),
                ram_mib: DEFAULT_RAM_MIB,
                backend: GuestBackend::Vm,
                root: None,
                boot_cache: None,
                answers: Vec::new(),
                hold_hooks: false,
            },
        );
        let kill = action(FaultOperation::Kill(1, ticks(256)));
        assert_eq!(
            workload.simpler_actions(&fixture_run(), &kill),
            vec![
                action(FaultOperation::Wait(ticks(256))),
                action(FaultOperation::Kill(1, ticks(128))),
            ]
        );
        assert_eq!(
            simpler_fault_actions(&action(FaultOperation::Wait(ticks(5)))),
            vec![action(FaultOperation::Wait(ticks(2)))]
        );
        assert!(simpler_fault_actions(&action(FaultOperation::Wait(ticks(1)))).is_empty());
        assert_eq!(
            simpler_fault_actions(&action(FaultOperation::KillAll(ticks(1)))),
            vec![action(FaultOperation::Wait(ticks(1)))]
        );
    }

    #[test]
    fn the_goal_tail_is_the_replay_settlement_waits_at_the_last_action_quantum() {
        let tail = settle_tail(Some(&action(FaultOperation::Kill(1, ticks(256)))));
        assert_eq!(tail.len(), crate::package::REPLAY_SETTLE_TICKS.len());
        assert!(
            tail.iter()
                .all(|wait| !wait.is_fault() && wait.coverage_quantum == ticks(8))
        );
        assert_eq!(tail.iter().map(FaultAction::ticks).sum::<u64>(), 8_191);
    }
}
