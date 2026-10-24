// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RecoveryError {
    #[error("recovery generation exhausted")]
    GenerationExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryReadiness {
    generation: u64,
    first_unready: u64,
    ready: bool,
    fault_pending: bool,
    queued: Vec<u32>,
}

impl RecoveryReadiness {
    #[must_use]
    pub fn initially_ready() -> Self {
        Self {
            generation: 0,
            first_unready: 0,
            ready: true,
            fault_pending: false,
            queued: Vec::new(),
        }
    }

    pub fn restarted(&mut self, after_fault: bool) -> Result<(), RecoveryError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(RecoveryError::GenerationExhausted)?;
        if self.ready {
            self.first_unready = self.generation;
        }
        self.ready = false;
        self.fault_pending |= after_fault;
        Ok(())
    }

    pub fn request_hook(&mut self, id: u32) -> Vec<u32> {
        if self.ready {
            vec![id]
        } else {
            self.queued.push(id);
            Vec::new()
        }
    }

    pub fn mark_ready(&mut self, generation: u64) -> Vec<u32> {
        if generation != self.generation || self.ready {
            return Vec::new();
        }
        self.ready = true;
        self.fault_pending = false;
        self.queued.drain(..).collect()
    }

    pub fn cancel_queued(&mut self, cancel: impl Fn(u32) -> bool) {
        self.queued.retain(|&id| !cancel(id));
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        !self.ready
    }

    #[must_use]
    pub fn is_fault_pending(&self) -> bool {
        self.fault_pending
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn first_unready(&self) -> u64 {
        self.first_unready
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HookFaults {
    heal: Option<u32>,
    launches: u64,
    unhealed: Option<u64>,
    running_cuts: u64,
    blocked_heals: Vec<u64>,
    heal_due: bool,
}

impl HookFaults {
    #[must_use]
    pub fn new(heal: Option<u32>) -> Self {
        Self {
            heal,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn is_cut(&self, id: u32) -> bool {
        self.heal.is_some_and(|heal| heal != id)
    }

    pub fn launched(&mut self, id: u32) -> u64 {
        let launch = self.launches;
        self.launches = self.launches.saturating_add(1);
        if self.is_cut(id) {
            self.unhealed = Some(launch);
            self.running_cuts = self.running_cuts.saturating_add(1);
            self.heal_due = false;
        } else if self.heal == Some(id) && self.running_cuts > 0 {
            self.blocked_heals.push(launch);
        }
        launch
    }

    pub fn finished(&mut self, id: u32, launch: u64, succeeded: bool) -> bool {
        if self.is_cut(id) {
            self.running_cuts = self.running_cuts.saturating_sub(1);
        }
        let blocked = self.heal == Some(id)
            && self
                .blocked_heals
                .iter()
                .position(|&blocked| blocked == launch)
                .map(|index| self.blocked_heals.swap_remove(index))
                .is_some();
        if succeeded
            && !blocked
            && self.heal == Some(id)
            && self.unhealed.is_some_and(|unhealed| unhealed < launch)
        {
            self.unhealed = None;
            return true;
        }
        false
    }

    pub fn cut_windows_closed(&mut self) {
        self.heal_due = self.heal.is_some();
    }

    pub fn due_heal(&mut self) -> Option<u32> {
        if !self.heal_due || self.running_cuts > 0 {
            return None;
        }
        self.heal_due = false;
        self.heal.filter(|_| self.in_force())
    }

    #[must_use]
    pub fn in_force(&self) -> bool {
        self.unhealed.is_some()
    }
}

pub const RECOVERY_NANOS: u64 = 30_000_000_000;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveryWatch {
    quiet_since: u64,
    unready_since: Option<(u64, u64)>,
    reported: Option<u64>,
}

impl RecoveryWatch {
    pub fn note_disturbed_until(&mut self, moment: u64) {
        self.quiet_since = self.quiet_since.max(moment);
    }

    pub fn observe(
        &mut self,
        moment: u64,
        disturbed: bool,
        readiness: &RecoveryReadiness,
    ) -> Option<u64> {
        if disturbed {
            self.quiet_since = moment;
        }
        if !readiness.is_pending() {
            return None;
        }
        let unready_since = match self.unready_since {
            Some((generation, since)) if generation == readiness.first_unready() => since,
            _ => {
                self.unready_since = Some((readiness.first_unready(), moment));
                moment
            }
        };
        if disturbed || self.reported == Some(readiness.generation()) {
            return None;
        }
        let quiet = moment.saturating_sub(self.quiet_since.max(unready_since));
        if quiet < RECOVERY_NANOS {
            return None;
        }
        self.reported = Some(readiness.generation());
        Some(quiet)
    }
}

#[cfg(test)]
mod tests {
    use super::{HookFaults, RecoveryError, RecoveryReadiness, RecoveryWatch};

    const STEP: u64 = crate::TICK_NANOS;
    const BOUND: u64 = super::RECOVERY_NANOS / STEP;

    #[test]
    fn a_workload_that_stays_unready_after_faults_end_is_reported_once() {
        let mut readiness = RecoveryReadiness::initially_ready();
        let mut watch = RecoveryWatch::default();
        readiness.restarted(true).unwrap();
        assert_eq!(watch.observe(10 * STEP, true, &readiness), None);
        assert_eq!(
            watch.observe((10 + BOUND - 1) * STEP, false, &readiness),
            None
        );
        assert_eq!(
            watch.observe((10 + BOUND) * STEP, false, &readiness),
            Some(BOUND * STEP)
        );
        assert_eq!(watch.observe((20 + BOUND) * STEP, false, &readiness), None);
        readiness.restarted(false).unwrap();
        assert_eq!(
            watch.observe((30 + BOUND) * STEP, false, &readiness),
            Some((20 + BOUND) * STEP)
        );
    }

    #[test]
    fn a_fault_replayed_between_polls_restarts_the_quiet_time_at_its_end() {
        let mut readiness = RecoveryReadiness::initially_ready();
        let mut watch = RecoveryWatch::default();
        readiness.restarted(true).unwrap();
        assert_eq!(watch.observe(0, true, &readiness), None);
        assert_eq!(watch.observe(29 * 1_000_000_000, false, &readiness), None);
        let ended = 29_900_000_000;
        watch.note_disturbed_until(ended);
        assert_eq!(watch.observe(30_200_000_000, false, &readiness), None);
        assert_eq!(
            watch.observe(ended + super::RECOVERY_NANOS, false, &readiness),
            Some(super::RECOVERY_NANOS)
        );
    }

    #[test]
    fn the_bound_counts_guest_time_however_many_polls_it_took() {
        let mut readiness = RecoveryReadiness::initially_ready();
        let mut watch = RecoveryWatch::default();
        readiness.restarted(true).unwrap();
        assert_eq!(watch.observe(7, true, &readiness), None);
        assert_eq!(
            watch.observe(7 + super::RECOVERY_NANOS - 1, false, &readiness),
            None
        );
        assert_eq!(
            watch.observe(7 + super::RECOVERY_NANOS, false, &readiness),
            Some(super::RECOVERY_NANOS)
        );
    }

    #[test]
    fn a_workload_that_loses_readiness_after_faults_end_gets_the_whole_bound() {
        let mut readiness = RecoveryReadiness::initially_ready();
        let mut watch = RecoveryWatch::default();
        readiness.restarted(true).unwrap();
        assert_eq!(watch.observe(10 * STEP, true, &readiness), None);
        assert_eq!(watch.observe(20 * STEP, false, &readiness), None);
        readiness.mark_ready(readiness.generation());
        assert_eq!(watch.observe(30 * STEP, false, &readiness), None);
        let mut tick = 10 + BOUND;
        for _ in 0..4 {
            readiness.restarted(true).unwrap();
            assert_eq!(watch.observe(tick * STEP, false, &readiness), None);
            assert_eq!(
                watch.observe((tick + BOUND - 1) * STEP, false, &readiness),
                None
            );
            readiness.mark_ready(readiness.generation());
            tick += BOUND;
        }
        readiness.restarted(true).unwrap();
        assert_eq!(watch.observe(tick * STEP, false, &readiness), None);
        readiness.restarted(true).unwrap();
        assert_eq!(
            watch.observe((tick + BOUND - 1) * STEP, false, &readiness),
            None
        );
        assert_eq!(
            watch.observe((tick + BOUND) * STEP, false, &readiness),
            Some(BOUND * STEP)
        );
    }

    #[test]
    fn a_hook_fault_stays_in_force_until_a_later_heal_succeeds() {
        let mut faults = HookFaults::new(Some(4));
        assert!(!faults.in_force());
        let early_heal = faults.launched(4);
        assert!(!faults.in_force());
        let partition = faults.launched(1);
        assert!(faults.in_force());
        faults.finished(1, partition, true);
        faults.finished(4, early_heal, true);
        assert!(faults.in_force());
        let failed_heal = faults.launched(4);
        faults.finished(4, failed_heal, false);
        assert!(faults.in_force());
        let heal = faults.launched(4);
        let later_cut = faults.launched(3);
        faults.finished(4, heal, true);
        assert!(faults.in_force());
        faults.finished(3, later_cut, true);
        let heal = faults.launched(4);
        faults.finished(4, heal, true);
        assert!(!faults.in_force());
    }

    #[test]
    fn hooks_leave_no_fault_in_force_without_a_heal_hook() {
        let mut faults = HookFaults::new(None);
        let launch = faults.launched(1);
        assert!(!faults.in_force());
        faults.finished(1, launch, true);
        assert!(!faults.in_force());
    }

    #[test]
    fn a_closed_cut_window_heals_after_the_cut_hook_exits() {
        let mut faults = HookFaults::new(Some(4));
        let cut = faults.launched(2);
        faults.cut_windows_closed();
        assert_eq!(faults.due_heal(), None);
        assert!(!faults.finished(2, cut, true));
        assert_eq!(faults.due_heal(), Some(4));
        assert_eq!(faults.due_heal(), None);
        let heal = faults.launched(4);
        assert!(faults.in_force());
        assert!(faults.finished(4, heal, true));
        assert!(!faults.in_force());
    }

    #[test]
    fn a_cut_already_healed_needs_no_heal_at_window_close() {
        let mut faults = HookFaults::new(Some(4));
        let cut = faults.launched(1);
        faults.finished(1, cut, true);
        let heal = faults.launched(4);
        assert!(faults.finished(4, heal, true));
        faults.cut_windows_closed();
        assert_eq!(faults.due_heal(), None);
    }

    #[test]
    fn a_new_cut_cancels_the_heal_a_closed_window_left_due() {
        let mut faults = HookFaults::new(Some(4));
        let first = faults.launched(1);
        faults.cut_windows_closed();
        let second = faults.launched(2);
        faults.finished(1, first, true);
        faults.finished(2, second, true);
        assert_eq!(faults.due_heal(), None);
        faults.cut_windows_closed();
        assert_eq!(faults.due_heal(), Some(4));
    }

    #[test]
    fn a_heal_that_finishes_while_a_cut_still_runs_leaves_the_cut_in_force() {
        let mut faults = HookFaults::new(Some(4));
        let cut = faults.launched(2);
        let heal = faults.launched(4);
        assert!(!faults.finished(4, heal, true));
        assert!(faults.in_force());
        assert!(!faults.finished(2, cut, true));
        faults.cut_windows_closed();
        assert_eq!(faults.due_heal(), Some(4));
    }

    #[test]
    fn a_heal_launched_while_a_cut_runs_never_clears_it_whatever_the_exit_order() {
        let mut faults = HookFaults::new(Some(4));
        let cut = faults.launched(2);
        let heal = faults.launched(4);
        assert!(!faults.finished(2, cut, true));
        assert!(!faults.finished(4, heal, true));
        assert!(faults.in_force());
        faults.cut_windows_closed();
        assert_eq!(faults.due_heal(), Some(4));
        let late = faults.launched(4);
        assert!(faults.finished(4, late, true));
        assert!(!faults.in_force());
    }

    #[test]
    fn a_failed_heal_leaves_the_cut_in_force() {
        let mut faults = HookFaults::new(Some(4));
        let cut = faults.launched(2);
        faults.finished(2, cut, true);
        faults.cut_windows_closed();
        assert_eq!(faults.due_heal(), Some(4));
        let heal = faults.launched(4);
        assert!(!faults.finished(4, heal, false));
        assert!(faults.in_force());
    }

    #[test]
    fn a_bundle_without_a_heal_hook_has_no_cuts_to_heal() {
        let mut faults = HookFaults::new(None);
        let hook = faults.launched(2);
        assert!(!faults.is_cut(2));
        faults.finished(2, hook, true);
        faults.cut_windows_closed();
        assert_eq!(faults.due_heal(), None);
    }

    #[test]
    fn a_cut_still_queued_when_its_window_closes_is_dropped() {
        let faults = HookFaults::new(Some(4));
        let mut readiness = RecoveryReadiness::initially_ready();
        readiness.restarted(true).unwrap();
        assert!(readiness.request_hook(2).is_empty());
        assert!(readiness.request_hook(4).is_empty());
        assert!(readiness.request_hook(1).is_empty());
        readiness.cancel_queued(|id| faults.is_cut(id));
        assert_eq!(readiness.mark_ready(readiness.generation()), [4]);
    }

    #[test]
    fn an_unhealed_hook_keeps_an_unready_workload_from_being_reported() {
        let mut readiness = RecoveryReadiness::initially_ready();
        let mut watch = RecoveryWatch::default();
        let mut faults = HookFaults::new(Some(4));
        let cut = faults.launched(1);
        faults.finished(1, cut, true);
        readiness.restarted(true).unwrap();
        for tick in 0..3 * BOUND {
            assert_eq!(
                watch.observe(tick * STEP, faults.in_force(), &readiness),
                None
            );
        }
        let heal = faults.launched(4);
        faults.finished(4, heal, true);
        assert_eq!(
            watch.observe((4 * BOUND - 2) * STEP, faults.in_force(), &readiness),
            None
        );
        assert_eq!(
            watch.observe((4 * BOUND - 1) * STEP, faults.in_force(), &readiness),
            Some(BOUND * STEP)
        );
    }

    #[test]
    fn faults_and_readiness_keep_the_watch_silent() {
        let mut readiness = RecoveryReadiness::initially_ready();
        let mut watch = RecoveryWatch::default();
        assert_eq!(watch.observe((2 * BOUND) * STEP, false, &readiness), None);
        readiness.restarted(true).unwrap();
        for tick in 0..3 * BOUND {
            assert_eq!(watch.observe(tick * STEP, true, &readiness), None);
        }
        readiness.mark_ready(readiness.generation());
        assert_eq!(watch.observe((10 * BOUND) * STEP, false, &readiness), None);
    }

    #[test]
    fn a_slow_probe_keeps_hooks_queued_until_the_current_generation_is_ready() {
        let mut readiness = RecoveryReadiness::initially_ready();
        readiness.restarted(true).unwrap();
        let generation = readiness.generation();

        assert!(readiness.is_pending());
        assert!(readiness.request_hook(7).is_empty());
        assert!(readiness.request_hook(7).is_empty());
        assert!(readiness.mark_ready(generation - 1).is_empty());
        assert!(readiness.is_pending());

        assert_eq!(readiness.mark_ready(generation), [7, 7]);
        assert!(!readiness.is_pending());
    }

    #[test]
    fn a_stale_probe_cannot_release_hooks_after_a_new_restart() {
        let mut readiness = RecoveryReadiness::initially_ready();
        readiness.restarted(true).unwrap();
        let stale = readiness.generation();
        assert!(readiness.request_hook(3).is_empty());

        readiness.restarted(true).unwrap();
        let current = readiness.generation();
        assert_ne!(current, stale);
        assert!(readiness.mark_ready(stale).is_empty());
        assert!(readiness.is_pending());
        assert_eq!(readiness.mark_ready(current), [3]);
    }

    #[test]
    fn ready_hooks_preserve_request_order_and_duplicates() {
        let mut readiness = RecoveryReadiness::initially_ready();
        readiness.restarted(true).unwrap();
        assert!(readiness.request_hook(2).is_empty());
        assert!(readiness.request_hook(1).is_empty());
        assert!(readiness.request_hook(2).is_empty());
        let generation = readiness.generation();

        assert_eq!(readiness.mark_ready(generation), [2, 1, 2]);
    }

    #[test]
    fn a_bundle_without_a_ready_probe_stays_immediately_ready() {
        let mut readiness = RecoveryReadiness::initially_ready();

        assert_eq!(readiness.request_hook(3), [3]);
        assert_eq!(readiness.request_hook(4), [4]);
    }

    #[test]
    fn natural_restart_does_not_exempt_crashes_but_fault_recovery_survives_restarts() {
        let mut readiness = RecoveryReadiness::initially_ready();
        readiness.restarted(false).unwrap();
        assert!(readiness.is_pending());
        assert!(!readiness.is_fault_pending());

        readiness.restarted(true).unwrap();
        let fault_generation = readiness.generation();
        readiness.restarted(false).unwrap();
        assert!(readiness.mark_ready(fault_generation).is_empty());
        assert!(readiness.is_fault_pending());
        readiness.mark_ready(readiness.generation());
        assert!(!readiness.is_fault_pending());
    }

    #[test]
    fn generation_exhaustion_does_not_reuse_a_stale_generation() {
        let mut readiness = RecoveryReadiness {
            generation: u64::MAX,
            first_unready: 0,
            ready: true,
            fault_pending: false,
            queued: Vec::new(),
        };

        assert_eq!(
            readiness.restarted(true),
            Err(RecoveryError::GenerationExhausted)
        );
        assert_eq!(readiness.generation(), u64::MAX);
        assert!(!readiness.is_pending());
    }
}
