// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::disk::DiskMode;
use crate::evidence::CheckEvidence;
use crate::reconcile::{ActiveWindows, EventKillWindow, EventPark, Standing};
use crate::regs::RegisterSnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Kill(u16),
    PowerLoss { node: u16, choice: u64, kill: bool },
    SetDiskMode { node: u16, mode: DiskMode },
    DiskCorrupt { node: u16, choice: u64, kill: bool },
    Stop(u16),
    Cont(u16),
    Start { node: u16, after_fault: bool },
    RunHook(u32),
    ArmEventKill(u16, u8, u64),
    DisarmEventKill(u16),
    ArmEventPark(u16, EventPark),
    DisarmEventPark(u16),
    SetClock { node: u16, offset_nanos: i64 },
}

impl Action {
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Action::Kill(node) => format!("kill node {node}"),
            Action::PowerLoss { node, choice, .. } => {
                format!("power loss node {node} choice {choice:#x}")
            }
            Action::SetDiskMode { node, mode } => {
                format!("disk node {node} {}", mode.describe())
            }
            Action::DiskCorrupt { node, choice, .. } => {
                format!("corrupt disk node {node} choice {choice:#x}")
            }
            Action::Stop(node) => format!("pause node {node}"),
            Action::Cont(node) => format!("resume node {node}"),
            Action::Start { node, .. } => format!("start node {node}"),
            Action::RunHook(id) => format!("run hook {id}"),
            Action::ArmEventKill(node, rarity, _) => {
                format!("arm event kill node {node} rarity {rarity}")
            }
            Action::DisarmEventKill(node) => format!("disarm event kill node {node}"),
            Action::ArmEventPark(node, park) => {
                let mut text = format!(
                    "arm event park node {node} edges {} hold {}",
                    park.edges, park.hold_nanos
                );
                if let Some(target) = park.target {
                    text.push_str(&format!(" target {:#x}..{:#x}", target.start, target.end));
                }
                text
            }
            Action::DisarmEventPark(node) => format!("disarm event park node {node}"),
            Action::SetClock { node, offset_nanos } => {
                format!("shift clock node {node} by {offset_nanos} ns")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counters {
    pub ticks: u64,
    pub hooks_started: u64,
    pub hooks_finished: u64,
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
    pub completed_check_pid: u64,
    pub pending_faults: u64,
    pub edge_crossings: u64,
    pub edge_digest: u64,
    pub event_park_held: u64,
}

#[derive(Clone, Copy, Debug)]
struct NodeState {
    alive: bool,
    paused: bool,
    expected_down: bool,
    event_kill_death: bool,
    restart_after_fault: bool,
    disk_faulted: bool,
}

#[derive(Clone, Debug)]
pub struct ProcessSupervisor {
    nodes: Vec<NodeState>,
    previous: ActiveWindows,
    counters: Counters,
    event_kill_selected: Vec<Option<EventKillWindow>>,
    event_kill_armed: Vec<EventKillWindow>,
    event_kill_fired: Vec<EventKillWindow>,
    natural_deaths: Vec<u16>,
    recovery_pending: bool,
    clocks: Vec<i64>,
    moment: Option<u64>,
}

pub type Supervisor = ProcessSupervisor;

impl ProcessSupervisor {
    #[must_use]
    pub fn new(node_count: usize) -> Self {
        assert!(node_count <= crate::bundle::MAX_NODES);
        Self {
            nodes: vec![
                NodeState {
                    alive: true,
                    paused: false,
                    expected_down: false,
                    event_kill_death: false,
                    restart_after_fault: false,
                    disk_faulted: false,
                };
                node_count
            ],
            previous: ActiveWindows::new(),
            counters: Counters::default(),
            event_kill_selected: vec![None; node_count],
            event_kill_armed: Vec::new(),
            event_kill_fired: Vec::new(),
            natural_deaths: Vec::new(),
            recovery_pending: false,
            clocks: vec![0; node_count],
            moment: None,
        }
    }

    pub fn take_natural_deaths(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.natural_deaths)
    }

    pub fn note_recovery_pending(&mut self, pending: bool) {
        self.recovery_pending = pending;
    }

    #[must_use]
    pub fn moment(&self) -> Option<u64> {
        self.moment
    }

    #[must_use]
    pub fn active(&self) -> &ActiveWindows {
        &self.previous
    }

    pub fn poll(&mut self, standing: &Standing, deaths: &[u16]) -> Vec<Action> {
        let steps = standing.steps(self.moment);
        self.moment = Some(standing.moment);
        self.tick_steps(&steps, deaths)
    }

    pub fn tick(&mut self, active: &ActiveWindows, deaths: &[u16]) -> Vec<Action> {
        self.tick_steps(std::slice::from_ref(active), deaths)
    }

    fn tick_steps(&mut self, steps: &[ActiveWindows], deaths: &[u16]) -> Vec<Action> {
        self.counters.ticks += 1;
        for &node in deaths {
            let natural = {
                let Some(state) = self.nodes.get_mut(usize::from(node)) else {
                    continue;
                };
                let event_kill_death = state.event_kill_death;
                state.event_kill_death = false;
                if !state.alive {
                    continue;
                }
                let natural = !state.expected_down && !event_kill_death && !state.disk_faulted;
                state.restart_after_fault =
                    state.expected_down || event_kill_death || state.disk_faulted;
                state.alive = false;
                state.paused = false;
                natural
            };
            if natural {
                self.bump_disturbance(1);
                self.counters.unexpected_deaths += 1;
                if !self.recovery_pending {
                    self.natural_deaths.push(node);
                }
            }
        }

        let mut actions = Vec::new();
        for active in steps {
            self.step(active, &mut actions);
        }
        actions
    }

    fn step(&mut self, active: &ActiveWindows, actions: &mut Vec<Action>) {
        for index in 0..self.nodes.len() {
            let node = index as u16;
            let was = self.previous.node(node);
            let now = active.node(node);
            let was_event_kill = self.event_kill_selected[index];
            let now_event_kill = active.event_kill(node, &self.event_kill_fired);
            let was_event_kill_fired = was_event_kill
                .is_some_and(|window| self.event_kill_fired.binary_search(&window).is_ok());
            self.event_kill_selected[index] = now_event_kill;
            let state = &mut self.nodes[index];

            if (now.kill && !was.kill) || (now.restart && !was.restart) {
                if state.alive {
                    actions.push(Action::Kill(node));
                    state.alive = false;
                    state.paused = false;
                    state.restart_after_fault = true;
                }
                state.expected_down = true;
            }
            if let Some(window) = now.power_loss
                && was.power_loss != now.power_loss
            {
                actions.push(Action::PowerLoss {
                    node,
                    choice: window.choice,
                    kill: state.alive,
                });
                if state.alive {
                    state.alive = false;
                    state.paused = false;
                    state.restart_after_fault = true;
                }
                state.expected_down = true;
            }
            if let Some(window) = now.disk_corrupt
                && was.disk_corrupt != now.disk_corrupt
            {
                actions.push(Action::DiskCorrupt {
                    node,
                    choice: window.choice,
                    kill: state.alive,
                });
                if state.alive {
                    state.alive = false;
                    state.paused = false;
                    state.restart_after_fault = true;
                }
                state.expected_down = true;
                state.disk_faulted = true;
            }
            if now.disk_mode() != was.disk_mode() {
                actions.push(Action::SetDiskMode {
                    node,
                    mode: now.disk_mode(),
                });
                state.disk_faulted |= now.disk_mode() != DiskMode::Normal;
            }
            if now.pause && !was.pause && state.alive && !state.paused {
                actions.push(Action::Stop(node));
                state.paused = true;
            }
            if !now.pause && was.pause && state.alive && state.paused {
                actions.push(Action::Cont(node));
                state.paused = false;
            }
            if state.alive {
                if !state.event_kill_death {
                    match (was_event_kill, now_event_kill) {
                        (None, Some(window)) => {
                            actions.push(Action::ArmEventKill(node, window.rarity, window.start));
                        }
                        (Some(_), None) if !was_event_kill_fired => {
                            actions.push(Action::DisarmEventKill(node));
                        }
                        (Some(previous), Some(window)) if previous != window => {
                            if !was_event_kill_fired {
                                actions.push(Action::DisarmEventKill(node));
                            }
                            actions.push(Action::ArmEventKill(node, window.rarity, window.start));
                        }
                        _ => {}
                    }
                }
                match (was.event_park, now.event_park) {
                    (None, Some(park)) => actions.push(Action::ArmEventPark(node, park)),
                    (Some(_), None) => actions.push(Action::DisarmEventPark(node)),
                    (Some(previous), Some(park)) if previous != park => {
                        actions.push(Action::DisarmEventPark(node));
                        actions.push(Action::ArmEventPark(node, park));
                    }
                    _ => {}
                }
            }
            if !now.restarting()
                && was.restarting()
                && !now.kill
                && now.power_loss.is_none()
                && !state.alive
            {
                actions.push(Action::Start {
                    node,
                    after_fault: state.restart_after_fault,
                });
                state.alive = true;
                state.paused = false;
                state.expected_down = false;
                state.event_kill_death = false;
                state.restart_after_fault = false;
                self.counters.restarts += 1;
                if let Some(window) = now_event_kill {
                    actions.push(Action::ArmEventKill(node, window.rarity, window.start));
                }
                if let Some(park) = now.event_park {
                    actions.push(Action::ArmEventPark(node, park));
                }
            }
        }

        for window in active.hooks() {
            if self.previous.hooks().binary_search(window).is_err() {
                actions.push(Action::RunHook(window.id));
            }
        }

        for index in 0..self.nodes.len() {
            let node = index as u16;
            let state = &self.nodes[index];
            if state.alive || state.expected_down || active.node(node).any() {
                continue;
            }
            actions.push(Action::Start {
                node,
                after_fault: self.nodes[index].restart_after_fault,
            });
            self.nodes[index].alive = true;
            self.nodes[index].paused = false;
            self.nodes[index].event_kill_death = false;
            self.nodes[index].restart_after_fault = false;
            self.counters.restarts += 1;
            if let Some(window) = self.event_kill_selected[index] {
                actions.push(Action::ArmEventKill(node, window.rarity, window.start));
            }
            if let Some(park) = active.node(node).event_park {
                actions.push(Action::ArmEventPark(node, park));
            }
        }

        for index in 0..self.nodes.len() {
            let node = index as u16;
            if self.previous.node(node).clock.is_some() != active.node(node).clock.is_some() {
                self.bump_disturbance(1);
            }
            let offset_nanos = active.clock_offset(node);
            if offset_nanos == self.clocks[index] {
                continue;
            }
            self.clocks[index] = offset_nanos;
            if self.nodes[index].alive {
                actions.push(Action::SetClock { node, offset_nanos });
            }
        }

        self.counters.pending_faults = active.pending_process_faults(&self.event_kill_fired);
        self.previous = active.clone();
    }

    #[must_use]
    pub fn clock_offset(&self, node: u16) -> i64 {
        self.clocks.get(usize::from(node)).copied().unwrap_or(0)
    }

    pub fn note_killed_for_remount(&mut self, node: u16) {
        if let Some(state) = self.nodes.get_mut(usize::from(node)) {
            state.alive = false;
            state.paused = false;
            state.restart_after_fault = true;
        }
    }

    pub fn note_hook_started(&mut self) {
        self.counters.hooks_started += 1;
    }

    pub fn note_hook_finished(&mut self) {
        self.counters.hooks_finished += 1;
    }

    pub fn note_process_transition(&mut self) {
        self.bump_disturbance(1);
    }

    pub fn note_late_unexpected_death(&mut self) {
        self.bump_disturbance(1);
        self.counters.unexpected_deaths += 1;
    }

    pub fn note_event_kill_armed(&mut self, node: u16, rarity: u8, start: u64) {
        if usize::from(node) >= self.nodes.len()
            || rarity >= process_proto::events::EVENT_RARITY_LIMIT
        {
            return;
        }
        let window = EventKillWindow {
            node,
            rarity,
            start,
        };
        if self.event_kill_fired.binary_search(&window).is_ok() {
            return;
        }
        if let Err(at) = self.event_kill_armed.binary_search(&window) {
            self.event_kill_armed.insert(at, window);
        }
    }

    pub fn note_event_kill_disarmed(&mut self, node: u16, rarity: u8, start: u64) {
        let window = EventKillWindow {
            node,
            rarity,
            start,
        };
        if let Ok(at) = self.event_kill_armed.binary_search(&window) {
            self.event_kill_armed.remove(at);
        }
    }

    pub fn note_event_kill(&mut self, node: u16, rarity: u8, start: u64, site: u64) -> bool {
        if usize::from(node) >= self.nodes.len()
            || rarity >= process_proto::events::EVENT_RARITY_LIMIT
        {
            return false;
        }
        let window = EventKillWindow {
            node,
            rarity,
            start,
        };
        let Ok(at) = self.event_kill_armed.binary_search(&window) else {
            return false;
        };
        if self.event_kill_fired.binary_search(&window).is_ok() {
            return false;
        }
        self.event_kill_armed.remove(at);
        self.bump_disturbance(1);
        self.counters.event_kill_fires = self.counters.event_kill_fires.saturating_add(1);
        self.counters.event_kill_site = site;
        if self.event_kill_fired.binary_search(&window).is_err() {
            let at = self
                .event_kill_fired
                .binary_search(&window)
                .unwrap_or_else(|at| at);
            self.event_kill_fired.insert(at, window);
        }
        if let Some(state) = self.nodes.get_mut(usize::from(node)) {
            state.event_kill_death = true;
        }
        true
    }

    pub fn note_event_parked(&mut self, fires: u64) {
        self.counters.event_park_fires = self.counters.event_park_fires.saturating_add(fires);
        self.bump_disturbance(fires);
    }

    pub fn note_event_park_held(&mut self, node: u16, held: bool) {
        if node >= 64 {
            return;
        }
        let bit = 1_u64 << node;
        if held {
            self.counters.event_park_held |= bit;
        } else {
            self.counters.event_park_held &= !bit;
        }
    }

    pub fn note_edge_coverage(&mut self, crossings: u64, digest: u64) {
        self.counters.edge_crossings = self.counters.edge_crossings.saturating_add(crossings);
        self.counters.edge_digest = self.counters.edge_digest.wrapping_add(digest);
    }

    pub fn note_workload_started(&mut self) {
        self.counters.workload_started = self.counters.workload_started.saturating_add(1);
    }

    pub fn note_workload_finished(&mut self) {
        self.counters.workload_finished = self.counters.workload_finished.saturating_add(1);
    }

    pub fn note_check_started(&mut self) {
        self.counters.checks_started = self.counters.checks_started.saturating_add(1);
    }

    pub fn note_check_finished(&mut self) {
        self.counters.checks_finished = self.counters.checks_finished.saturating_add(1);
    }

    pub fn note_check_completed(&mut self, evidence: CheckEvidence) {
        self.counters.completed_check_run = evidence.run;
        self.counters.completed_check_start_generation = evidence.start_generation;
        self.counters.completed_check_end_generation = evidence.end_generation;
        self.counters.completed_check_pid = evidence.pid;
    }

    pub fn set_check_enabled(&mut self, enabled: bool) {
        self.counters.check_enabled = u64::from(enabled);
    }

    #[must_use]
    pub fn disturbance_generation(&self) -> u64 {
        self.counters.disturbance_generation
    }

    #[must_use]
    pub fn pending_faults(&self) -> u64 {
        self.counters.pending_faults
    }

    #[must_use]
    pub fn disk_faulted(&self, node: u16) -> bool {
        self.nodes
            .get(usize::from(node))
            .is_some_and(|state| state.disk_faulted)
    }

    #[must_use]
    pub fn any_node_held_down(&self) -> bool {
        self.nodes.iter().any(|state| state.expected_down)
    }

    pub fn set_pending_faults(&mut self, pending: u64) {
        self.counters.pending_faults = pending;
    }

    pub fn note_infrastructure_error(&mut self) -> bool {
        if self.counters.infrastructure_error != 0 {
            return false;
        }
        self.counters.infrastructure_error = 1;
        true
    }

    pub fn note_event_ready(&mut self, node: u16, ready: bool) {
        if node >= 64 {
            return;
        }
        let bit = 1_u64 << node;
        if ready {
            self.counters.event_ready |= bit;
        } else {
            self.counters.event_ready &= !bit;
        }
    }

    #[must_use]
    pub fn alive_bitmap(&self) -> u64 {
        let mut bits = 0;
        for (index, state) in self.nodes.iter().enumerate() {
            if state.alive {
                bits |= 1_u64 << index;
            }
        }
        bits
    }

    #[must_use]
    pub fn counters(&self) -> Counters {
        self.counters
    }

    #[must_use]
    pub fn snapshot(&self) -> RegisterSnapshot {
        RegisterSnapshot {
            ticks: self.counters.ticks,
            alive: self.alive_bitmap(),
            hooks_started: self.counters.hooks_started,
            hooks_finished: self.counters.hooks_finished,
            unexpected_deaths: self.counters.unexpected_deaths,
            restarts: self.counters.restarts,
            event_kill_fires: self.counters.event_kill_fires,
            event_kill_site: self.counters.event_kill_site,
            event_park_fires: self.counters.event_park_fires,
            workload_started: self.counters.workload_started,
            workload_finished: self.counters.workload_finished,
            checks_started: self.counters.checks_started,
            checks_finished: self.counters.checks_finished,
            infrastructure_error: self.counters.infrastructure_error,
            event_ready: self.counters.event_ready,
            disturbance_generation: self.counters.disturbance_generation,
            check_enabled: self.counters.check_enabled,
            completed_check_run: self.counters.completed_check_run,
            completed_check_start_generation: self.counters.completed_check_start_generation,
            completed_check_end_generation: self.counters.completed_check_end_generation,
            completed_check_pid: self.counters.completed_check_pid,
            pending_faults: self.counters.pending_faults,
            edge_crossings: self.counters.edge_crossings,
            edge_digest: self.counters.edge_digest,
            event_park_held: self.counters.event_park_held,
            event_kill_armed: self
                .event_kill_armed
                .iter()
                .filter(|window| window.node < 64)
                .fold(0, |bits, window| bits | 1_u64 << window.node)
                & self.alive_bitmap(),
        }
    }

    fn bump_disturbance(&mut self, amount: u64) {
        if amount == 0 {
            return;
        }
        if let Some(next) = self.counters.disturbance_generation.checked_add(amount) {
            self.counters.disturbance_generation = next;
        } else {
            self.counters.disturbance_generation = u64::MAX;
            self.counters.infrastructure_error = 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(run: u64, sup: &Supervisor) -> CheckEvidence {
        CheckEvidence {
            run,
            start_generation: sup.disturbance_generation(),
            end_generation: 0,
            pid: 7,
        }
    }

    fn completed(check: CheckEvidence, sup: &Supervisor) -> CheckEvidence {
        CheckEvidence {
            end_generation: sup.disturbance_generation(),
            ..check
        }
    }
    use process_proto::{EVERY_NODE, ProcessAction, ProcessWindow};

    fn active(actions: &[(u16, ProcessAction)]) -> ActiveWindows {
        let mut set = ActiveWindows::new();
        for (node, action) in actions {
            set.insert(*node, action, 0);
        }
        set
    }

    #[test]
    fn only_a_death_the_search_did_not_cause_is_natural() {
        let mut sup = Supervisor::new(2);
        let killed = active(&[(0, ProcessAction::Kill)]);
        sup.tick(&killed, &[]);
        sup.tick(&killed, &[0]);
        assert!(sup.take_natural_deaths().is_empty());
        sup.tick(&killed, &[1]);
        assert_eq!(sup.take_natural_deaths(), [1]);
        assert!(sup.take_natural_deaths().is_empty());
    }

    #[test]
    fn a_death_before_the_bundle_is_ready_again_is_not_natural() {
        let mut sup = Supervisor::new(1);
        sup.note_recovery_pending(true);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[0]),
            [Action::Start {
                node: 0,
                after_fault: false
            }]
        );
        assert!(sup.take_natural_deaths().is_empty());
        assert_eq!(sup.snapshot().unexpected_deaths, 1);
        sup.note_recovery_pending(false);
        sup.tick(&ActiveWindows::new(), &[0]);
        assert_eq!(sup.take_natural_deaths(), [0]);
    }

    #[test]
    fn a_kill_window_kills_once_and_the_node_stays_down() {
        let mut sup = Supervisor::new(2);
        let killed = active(&[(0, ProcessAction::Kill)]);
        assert_eq!(sup.tick(&killed, &[]), [Action::Kill(0)]);
        assert_eq!(sup.alive_bitmap(), 0b10);
        assert_eq!(sup.tick(&killed, &[]), []);
        assert_eq!(sup.tick(&killed, &[0]), []);
        assert_eq!(sup.tick(&ActiveWindows::new(), &[]), []);
        assert_eq!(sup.tick(&ActiveWindows::new(), &[]), []);
        assert_eq!(sup.alive_bitmap(), 0b10);
        assert!(sup.any_node_held_down());
        assert_eq!(sup.counters().unexpected_deaths, 0);
        assert_eq!(sup.counters().restarts, 0);
        assert_eq!(sup.counters().ticks, 5);
        let restart = active(&[(0, ProcessAction::Restart)]);
        assert_eq!(sup.tick(&restart, &[]), []);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [Action::Start {
                node: 0,
                after_fault: true
            }]
        );
        assert!(!sup.any_node_held_down());
    }

    #[test]
    fn a_power_loss_kills_once_resets_the_disk_and_holds_the_node_down() {
        let mut sup = Supervisor::new(2);
        let lost = active(&[(EVERY_NODE, ProcessAction::PowerLoss { choice: 7 })]);
        assert_eq!(
            sup.tick(&lost, &[]),
            [
                Action::PowerLoss {
                    node: 0,
                    choice: 7,
                    kill: true
                },
                Action::PowerLoss {
                    node: 1,
                    choice: 7,
                    kill: true
                }
            ]
        );
        assert_eq!(sup.alive_bitmap(), 0);
        assert_eq!(sup.tick(&lost, &[0, 1]), []);
        assert_eq!(sup.tick(&ActiveWindows::new(), &[]), []);
        assert!(sup.any_node_held_down());
        assert_eq!(sup.counters().unexpected_deaths, 0);
        let restart = active(&[(0, ProcessAction::Restart)]);
        assert_eq!(sup.tick(&restart, &[]), []);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [Action::Start {
                node: 0,
                after_fault: true
            }]
        );
        assert!(sup.any_node_held_down());
        assert_eq!(sup.alive_bitmap(), 0b01);
    }

    fn standing(moment: u64, windows: &[(u16, ProcessAction, u64, u64)]) -> Standing {
        Standing {
            moment,
            windows: windows
                .iter()
                .map(|&(node, action, start, end)| ProcessWindow {
                    node,
                    action,
                    start,
                    end,
                })
                .collect(),
        }
    }

    #[test]
    fn a_restart_that_opens_and_closes_during_a_rebuild_starts_the_node() {
        let mut sup = Supervisor::new(1);
        let windows = [
            (0, ProcessAction::PowerLoss { choice: 7 }, 10, 20),
            (0, ProcessAction::Restart, 40, 80),
        ];
        assert_eq!(sup.poll(&standing(5, &[]), &[]), []);
        assert_eq!(
            sup.poll(&standing(15, &windows), &[]),
            [Action::PowerLoss {
                node: 0,
                choice: 7,
                kill: true
            }]
        );
        assert_eq!(
            sup.poll(&standing(260, &windows), &[0]),
            [Action::Start {
                node: 0,
                after_fault: true
            }]
        );
        assert_eq!(sup.alive_bitmap(), 1);
        assert!(!sup.any_node_held_down());
        assert_eq!(sup.counters().unexpected_deaths, 0);
        assert_eq!(sup.counters().restarts, 1);
        assert_eq!(sup.poll(&standing(270, &[]), &[]), []);
    }

    #[test]
    fn another_nodes_faults_during_a_rebuild_take_effect_in_order() {
        let mut sup = Supervisor::new(2);
        let windows = [
            (0, ProcessAction::PowerLoss { choice: 3 }, 10, 20),
            (1, ProcessAction::Restart, 70, 90),
            (1, ProcessAction::PowerLoss { choice: 5 }, 40, 60),
            (1, ProcessAction::Pause(1), 100, 120),
        ];
        assert_eq!(sup.poll(&standing(5, &[]), &[]), []);
        assert_eq!(
            sup.poll(&standing(15, &windows), &[]),
            [Action::PowerLoss {
                node: 0,
                choice: 3,
                kill: true
            }]
        );
        assert_eq!(
            sup.poll(&standing(1_800, &windows), &[0]),
            [
                Action::PowerLoss {
                    node: 1,
                    choice: 5,
                    kill: true
                },
                Action::Start {
                    node: 1,
                    after_fault: true
                },
                Action::Stop(1),
                Action::Cont(1),
            ]
        );
        assert_eq!(sup.alive_bitmap(), 0b10);
        assert_eq!(sup.counters().unexpected_deaths, 0);
    }

    #[test]
    fn a_corruption_window_inside_another_nodes_corruption_still_corrupts() {
        let mut sup = Supervisor::new(2);
        let windows = [
            (0, ProcessAction::DiskCorrupt { choice: 4 }, 10, 20),
            (1, ProcessAction::DiskCorrupt { choice: 6 }, 30, 55),
        ];
        assert_eq!(sup.poll(&standing(5, &[]), &[]), []);
        assert_eq!(
            sup.poll(&standing(15, &windows), &[]),
            [Action::DiskCorrupt {
                node: 0,
                choice: 4,
                kill: true
            }]
        );
        assert_eq!(
            sup.poll(&standing(1_800, &windows), &[0]),
            [
                Action::Start {
                    node: 0,
                    after_fault: true
                },
                Action::DiskCorrupt {
                    node: 1,
                    choice: 6,
                    kill: true
                },
                Action::Start {
                    node: 1,
                    after_fault: true
                },
            ]
        );
        assert_eq!(sup.alive_bitmap(), 0b11);
        assert!(sup.disk_faulted(1));
    }

    #[test]
    fn back_to_back_power_losses_each_reset_the_disk() {
        let mut sup = Supervisor::new(1);
        let windows = [
            (0, ProcessAction::PowerLoss { choice: 1 }, 10, 20),
            (0, ProcessAction::PowerLoss { choice: 2 }, 20, 30),
            (0, ProcessAction::PowerLoss { choice: 3 }, 35, 45),
            (0, ProcessAction::Restart, 50, 60),
        ];
        assert_eq!(sup.poll(&standing(5, &[]), &[]), []);
        assert_eq!(
            sup.poll(&standing(15, &windows), &[]),
            [Action::PowerLoss {
                node: 0,
                choice: 1,
                kill: true
            }]
        );
        assert_eq!(
            sup.poll(&standing(32, &windows), &[0]),
            [Action::PowerLoss {
                node: 0,
                choice: 2,
                kill: false
            }]
        );
        assert_eq!(
            sup.poll(&standing(400, &windows), &[]),
            [
                Action::PowerLoss {
                    node: 0,
                    choice: 3,
                    kill: false
                },
                Action::Start {
                    node: 0,
                    after_fault: true
                }
            ]
        );
        assert_eq!(sup.alive_bitmap(), 1);
        assert_eq!(sup.counters().restarts, 1);
        assert_eq!(sup.counters().unexpected_deaths, 0);
    }

    #[test]
    fn an_event_kill_window_missed_between_polls_arms_with_its_own_start() {
        let mut sup = Supervisor::new(1);
        let windows = [(0, ProcessAction::EventKill { rarity: 2 }, 40, 60)];
        assert_eq!(sup.poll(&standing(15, &[]), &[]), []);
        assert_eq!(
            sup.poll(&standing(100, &windows), &[]),
            [Action::ArmEventKill(0, 2, 40), Action::DisarmEventKill(0)]
        );
        assert_eq!(sup.pending_faults(), 0);
        assert!(sup.active().is_empty());
    }

    #[test]
    fn the_first_poll_replays_no_window_that_already_closed() {
        let mut sup = Supervisor::new(1);
        let windows = [(0, ProcessAction::Kill, 10, 20)];
        assert_eq!(sup.moment(), None);
        assert_eq!(sup.poll(&standing(100, &windows), &[]), []);
        assert_eq!(sup.moment(), Some(100));
        assert_eq!(sup.alive_bitmap(), 1);
    }

    #[test]
    fn a_power_loss_on_a_dead_node_still_resets_its_disk() {
        let mut sup = Supervisor::new(1);
        let killed = active(&[(0, ProcessAction::Kill)]);
        assert_eq!(sup.tick(&killed, &[]), [Action::Kill(0)]);
        let mut both = active(&[(0, ProcessAction::Kill)]);
        both.insert(0, &ProcessAction::PowerLoss { choice: 3 }, 5);
        assert_eq!(
            sup.tick(&both, &[0]),
            [Action::PowerLoss {
                node: 0,
                choice: 3,
                kill: false
            }]
        );
        assert_eq!(sup.tick(&both, &[]), []);
        let mut later = active(&[(0, ProcessAction::Kill)]);
        later.insert(0, &ProcessAction::PowerLoss { choice: 3 }, 9);
        assert_eq!(
            sup.tick(&later, &[]),
            [Action::PowerLoss {
                node: 0,
                choice: 3,
                kill: false
            }]
        );
    }

    #[test]
    fn a_disk_corruption_kills_once_and_restarts_when_its_window_closes() {
        let mut sup = Supervisor::new(2);
        let corrupt = active(&[(1, ProcessAction::DiskCorrupt { choice: 5 })]);
        assert_eq!(
            sup.tick(&corrupt, &[]),
            [Action::DiskCorrupt {
                node: 1,
                choice: 5,
                kill: true
            }]
        );
        assert_eq!(sup.alive_bitmap(), 0b01);
        assert_eq!(sup.tick(&corrupt, &[1]), []);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [Action::Start {
                node: 1,
                after_fault: true
            }]
        );
        assert_eq!(sup.alive_bitmap(), 0b11);
        assert_eq!(sup.counters().unexpected_deaths, 0);
        assert!(sup.disk_faulted(1));
        assert!(!sup.disk_faulted(0));
    }

    #[test]
    fn disk_error_and_stall_windows_set_and_clear_the_disk_mode() {
        let mut sup = Supervisor::new(1);
        let error = active(&[(0, ProcessAction::DiskError)]);
        assert_eq!(
            sup.tick(&error, &[]),
            [Action::SetDiskMode {
                node: 0,
                mode: DiskMode::Error
            }]
        );
        assert!(sup.disk_faulted(0));
        let both = active(&[
            (0, ProcessAction::DiskError),
            (0, ProcessAction::DiskStall { hold_nanos: 70 }),
        ]);
        assert_eq!(sup.tick(&both, &[]), []);
        let stall = active(&[(0, ProcessAction::DiskStall { hold_nanos: 70 })]);
        assert_eq!(
            sup.tick(&stall, &[]),
            [Action::SetDiskMode {
                node: 0,
                mode: DiskMode::Stall { hold_nanos: 70 }
            }]
        );
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [Action::SetDiskMode {
                node: 0,
                mode: DiskMode::Normal
            }]
        );
        assert_eq!(sup.alive_bitmap(), 1);
        assert_eq!(sup.counters().restarts, 0);
    }

    #[test]
    fn a_node_that_exits_after_a_disk_fault_returns_when_the_window_closes() {
        let mut sup = Supervisor::new(1);
        let error = active(&[(0, ProcessAction::DiskError)]);
        sup.tick(&error, &[]);
        assert_eq!(sup.tick(&error, &[0]), []);
        assert!(sup.take_natural_deaths().is_empty());
        assert_eq!(sup.counters().unexpected_deaths, 0);
        assert_eq!(sup.tick(&error, &[]), []);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [
                Action::SetDiskMode {
                    node: 0,
                    mode: DiskMode::Normal
                },
                Action::Start {
                    node: 0,
                    after_fault: true
                }
            ]
        );
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[0]),
            [Action::Start {
                node: 0,
                after_fault: true
            }]
        );
        assert!(sup.take_natural_deaths().is_empty());
    }

    #[test]
    fn a_node_killed_for_a_remount_restarts_after_a_fault() {
        let mut sup = Supervisor::new(1);
        let error = active(&[(0, ProcessAction::DiskError)]);
        sup.tick(&error, &[]);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [Action::SetDiskMode {
                node: 0,
                mode: DiskMode::Normal
            }]
        );
        sup.note_killed_for_remount(0);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[0]),
            [Action::Start {
                node: 0,
                after_fault: true
            }]
        );
        assert!(sup.take_natural_deaths().is_empty());
        assert_eq!(sup.counters().unexpected_deaths, 0);
    }

    #[test]
    fn a_restart_closing_under_a_power_loss_leaves_the_node_down() {
        let mut sup = Supervisor::new(1);
        let both = active(&[
            (0, ProcessAction::Restart),
            (0, ProcessAction::PowerLoss { choice: 0 }),
        ]);
        assert_eq!(
            sup.tick(&both, &[]),
            [
                Action::Kill(0),
                Action::PowerLoss {
                    node: 0,
                    choice: 0,
                    kill: false
                }
            ]
        );
        let lost = active(&[(0, ProcessAction::PowerLoss { choice: 0 })]);
        assert_eq!(sup.tick(&lost, &[0]), []);
        assert_eq!(sup.tick(&ActiveWindows::new(), &[]), []);
        assert_eq!(sup.alive_bitmap(), 0);
        assert_eq!(sup.counters().restarts, 0);
    }

    #[test]
    fn a_pause_window_stops_on_entry_and_continues_on_exit() {
        let mut sup = Supervisor::new(1);
        let paused = active(&[(0, ProcessAction::Pause(30))]);
        assert_eq!(sup.tick(&paused, &[]), [Action::Stop(0)]);
        assert_eq!(sup.tick(&paused, &[]), []);
        assert_eq!(sup.tick(&ActiveWindows::new(), &[]), [Action::Cont(0)]);
        assert_eq!(sup.tick(&ActiveWindows::new(), &[]), []);
        assert_eq!(sup.alive_bitmap(), 1);
        assert_eq!(sup.counters().restarts, 0);
    }

    #[test]
    fn a_restart_window_kills_on_entry_and_starts_on_exit() {
        let mut sup = Supervisor::new(1);
        let restart = active(&[(0, ProcessAction::Restart)]);
        assert_eq!(sup.tick(&restart, &[]), [Action::Kill(0)]);
        assert_eq!(sup.alive_bitmap(), 0);
        assert_eq!(sup.tick(&restart, &[0]), []);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [Action::Start {
                node: 0,
                after_fault: true
            }]
        );
        assert_eq!(sup.alive_bitmap(), 1);
        assert_eq!(sup.counters().restarts, 1);
        assert_eq!(sup.counters().unexpected_deaths, 0);
    }

    #[test]
    fn a_death_reaped_before_the_restart_decision_does_not_make_recovery_fault_caused() {
        let mut sup = Supervisor::new(1);
        let restart = active(&[(0, ProcessAction::Restart)]);
        assert_eq!(sup.tick(&restart, &[0]), []);
        assert_eq!(sup.take_natural_deaths(), [0]);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [Action::Start {
                node: 0,
                after_fault: false,
            }]
        );
    }

    #[test]
    fn a_restart_closing_under_a_kill_leaves_the_node_down() {
        let mut sup = Supervisor::new(1);
        let both = active(&[(0, ProcessAction::Restart), (0, ProcessAction::Kill)]);
        assert_eq!(sup.tick(&both, &[]), [Action::Kill(0)]);
        let kill = active(&[(0, ProcessAction::Kill)]);
        assert_eq!(sup.tick(&kill, &[]), []);
        assert_eq!(sup.alive_bitmap(), 0);
        assert_eq!(sup.counters().restarts, 0);
    }

    #[test]
    fn a_pause_leaving_with_the_node_dead_emits_no_signal() {
        let mut sup = Supervisor::new(1);
        let paused = active(&[(0, ProcessAction::Pause(1))]);
        assert_eq!(sup.tick(&paused, &[]), [Action::Stop(0)]);
        assert_eq!(sup.tick(&paused, &[0]), []);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [Action::Start {
                node: 0,
                after_fault: false
            }]
        );
        assert_eq!(sup.counters().unexpected_deaths, 1);
        assert_eq!(sup.counters().restarts, 1);
    }

    #[test]
    fn an_unexpected_death_is_counted_and_restarted_next_tick() {
        let mut sup = Supervisor::new(2);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[1]),
            [Action::Start {
                node: 1,
                after_fault: false
            }]
        );
        assert_eq!(sup.alive_bitmap(), 0b11);
        assert_eq!(sup.counters().unexpected_deaths, 1);
        assert_eq!(sup.counters().restarts, 1);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[1, 1]),
            [Action::Start {
                node: 1,
                after_fault: false
            }]
        );
        assert_eq!(sup.counters().unexpected_deaths, 2);
    }

    #[test]
    fn a_death_under_a_pause_window_is_not_restarted_until_the_window_closes() {
        let mut sup = Supervisor::new(1);
        let paused = active(&[(0, ProcessAction::Pause(1))]);
        sup.tick(&paused, &[]);
        assert_eq!(sup.tick(&paused, &[0]), []);
        assert_eq!(sup.tick(&paused, &[]), []);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [Action::Start {
                node: 0,
                after_fault: false
            }]
        );
    }

    #[test]
    fn event_windows_arm_and_disarm_on_edges() {
        let mut sup = Supervisor::new(1);
        let event = active(&[
            (0, ProcessAction::EventKill { rarity: 2 }),
            (
                0,
                ProcessAction::EventPark {
                    edges: 3,
                    hold_nanos: 8,
                    target: None,
                },
            ),
        ]);
        assert_eq!(
            sup.tick(&event, &[]),
            [
                Action::ArmEventKill(0, 2, 0),
                Action::ArmEventPark(
                    0,
                    EventPark {
                        edges: 3,
                        hold_nanos: 8,
                        target: None,
                        start: 0,
                    },
                ),
            ]
        );
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [Action::DisarmEventKill(0), Action::DisarmEventPark(0)]
        );
    }

    #[test]
    fn touching_event_park_windows_rearm_with_their_new_start() {
        let mut sup = Supervisor::new(1);
        let mut first = ActiveWindows::new();
        first.insert(
            0,
            &ProcessAction::EventPark {
                edges: 3,
                hold_nanos: 10_000_000,
                target: None,
            },
            0,
        );
        let mut second = ActiveWindows::new();
        second.insert(
            0,
            &ProcessAction::EventPark {
                edges: 3,
                hold_nanos: 10_000_000,
                target: None,
            },
            500_000_000,
        );
        assert_eq!(
            sup.tick(&first, &[]),
            [Action::ArmEventPark(
                0,
                EventPark {
                    edges: 3,
                    hold_nanos: 10_000_000,
                    target: None,
                    start: 0,
                }
            )]
        );
        assert_eq!(
            sup.tick(&second, &[]),
            [
                Action::DisarmEventPark(0),
                Action::ArmEventPark(
                    0,
                    EventPark {
                        edges: 3,
                        hold_nanos: 10_000_000,
                        target: None,
                        start: 500_000_000,
                    }
                )
            ]
        );
        assert_eq!(sup.tick(&second, &[]), []);
    }

    #[test]
    fn an_event_kill_report_is_required_to_avoid_unexpected_death() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, ProcessAction::EventKill { rarity: 0 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 0, 0)]);
        assert_eq!(
            sup.tick(&event, &[0]),
            [
                Action::Start {
                    node: 0,
                    after_fault: false
                },
                Action::ArmEventKill(0, 0, 0)
            ]
        );
        assert_eq!(sup.counters().unexpected_deaths, 1);

        let mut sup = Supervisor::new(1);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 0, 0)]);
        sup.note_event_kill_armed(0, 0, 0);
        assert!(sup.note_event_kill(0, 0, 0, 0xfeed));
        assert_eq!(
            sup.tick(&event, &[0]),
            [Action::Start {
                node: 0,
                after_fault: true
            }]
        );
        assert_eq!(sup.counters().unexpected_deaths, 0);
        assert_eq!(sup.counters().event_kill_fires, 1);
        assert_eq!(sup.counters().event_kill_site, 0xfeed);
    }

    #[test]
    fn an_event_kill_report_survives_until_the_death_is_observed() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, ProcessAction::EventKill { rarity: 0 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 0, 0)]);
        sup.note_event_kill_armed(0, 0, 0);
        assert!(sup.note_event_kill(0, 0, 0, 0xfeed));
        assert_eq!(sup.tick(&event, &[]), []);
        assert_eq!(
            sup.tick(&event, &[0]),
            [Action::Start {
                node: 0,
                after_fault: true
            }]
        );
        assert_eq!(sup.counters().unexpected_deaths, 0);
    }

    #[test]
    fn an_event_kill_report_does_not_cross_an_incarnation_replacement() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, ProcessAction::EventKill { rarity: 0 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 0, 0)]);
        sup.note_event_kill_armed(0, 0, 0);
        assert!(sup.note_event_kill(0, 0, 0, 0xfeed));
        let restart = active(&[(0, ProcessAction::Restart)]);
        assert_eq!(sup.tick(&restart, &[]), [Action::Kill(0)]);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [Action::Start {
                node: 0,
                after_fault: true
            }]
        );
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[0]),
            [Action::Start {
                node: 0,
                after_fault: false
            }]
        );
        assert_eq!(sup.counters().unexpected_deaths, 1);
    }

    #[test]
    fn a_fired_event_kill_advances_to_the_next_window() {
        let mut sup = Supervisor::new(1);
        let mut events = ActiveWindows::new();
        events.insert(0, &ProcessAction::EventKill { rarity: 0 }, 0);
        events.insert(0, &ProcessAction::EventKill { rarity: 0 }, 500_000_000);

        assert_eq!(sup.tick(&events, &[]), [Action::ArmEventKill(0, 0, 0)]);
        sup.note_event_kill_armed(0, 0, 0);
        assert!(sup.note_event_kill(0, 0, 0, 0xfeed));
        assert_eq!(
            sup.tick(&events, &[0]),
            [
                Action::Start {
                    node: 0,
                    after_fault: true
                },
                Action::ArmEventKill(0, 0, 500_000_000)
            ]
        );
        assert_eq!(sup.pending_faults(), 1);

        sup.note_event_kill_armed(0, 0, 500_000_000);
        assert!(sup.note_event_kill(0, 0, 500_000_000, 0xcafe));
        assert_eq!(
            sup.tick(&events, &[0]),
            [Action::Start {
                node: 0,
                after_fault: true
            }]
        );
        assert_eq!(sup.pending_faults(), 0);
        assert_eq!(sup.counters().event_kill_fires, 2);
    }

    #[test]
    fn restarting_a_node_rearms_an_event_fault_that_stayed_active() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, ProcessAction::EventKill { rarity: 1 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 1, 0)]);
        assert_eq!(
            sup.tick(&event, &[0]),
            [
                Action::Start {
                    node: 0,
                    after_fault: false
                },
                Action::ArmEventKill(0, 1, 0)
            ]
        );
    }

    #[test]
    fn a_report_for_an_obsolete_arm_does_not_credit_the_current_incarnation() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, ProcessAction::EventKill { rarity: 4 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 4, 0)]);
        assert!(!sup.note_event_kill(0, 4, 9, 0x10));
        assert_eq!(
            sup.tick(&event, &[0]),
            [
                Action::Start {
                    node: 0,
                    after_fault: false
                },
                Action::ArmEventKill(0, 4, 0)
            ]
        );
        assert_eq!(sup.counters().event_kill_fires, 0);
        assert_eq!(sup.counters().unexpected_deaths, 1);
        assert_eq!(sup.counters().disturbance_generation, 1);
    }

    #[test]
    fn an_acknowledged_old_window_is_valid_after_the_current_window_changes() {
        let mut sup = Supervisor::new(1);
        let first = active(&[(0, ProcessAction::EventKill { rarity: 4 })]);
        assert_eq!(sup.tick(&first, &[]), [Action::ArmEventKill(0, 4, 0)]);
        sup.note_event_kill_armed(0, 4, 0);

        let mut second = ActiveWindows::new();
        second.insert(0, &ProcessAction::EventKill { rarity: 4 }, 1);
        assert_eq!(
            sup.tick(&second, &[]),
            [Action::DisarmEventKill(0), Action::ArmEventKill(0, 4, 1)]
        );
        assert!(sup.note_event_kill(0, 4, 0, 0x10));
        assert_eq!(sup.counters().event_kill_fires, 1);
        assert_eq!(sup.counters().disturbance_generation, 1);
        assert!(!sup.note_event_kill(0, 4, 0, 0x10));
        assert_eq!(sup.counters().disturbance_generation, 1);
    }

    #[test]
    fn hooks_launch_once_per_window_and_ids_are_ascending() {
        let mut sup = Supervisor::new(1);
        let two = active(&[
            (0, ProcessAction::RunHook(4)),
            (0, ProcessAction::RunHook(1)),
        ]);
        assert_eq!(
            sup.tick(&two, &[]),
            [Action::RunHook(1), Action::RunHook(4)]
        );
        assert_eq!(sup.tick(&two, &[]), []);
        let next = active(&[
            (0, ProcessAction::RunHook(4)),
            (0, ProcessAction::RunHook(7)),
        ]);
        assert_eq!(sup.tick(&next, &[]), [Action::RunHook(7)]);
        assert_eq!(sup.tick(&two, &[]), [Action::RunHook(1)]);
        assert_eq!(sup.counters().hooks_started, 0);
        for _ in 0..4 {
            sup.note_hook_started();
        }
        assert_eq!(sup.counters().hooks_started, 4);
    }

    #[test]
    fn a_requested_hook_is_not_counted_until_it_is_started() {
        let mut sup = Supervisor::new(1);
        assert_eq!(
            sup.tick(&active(&[(0, ProcessAction::RunHook(1))]), &[]),
            [Action::RunHook(1)]
        );
        assert_eq!(sup.counters().hooks_started, 0);
        assert_eq!(sup.counters().hooks_finished, 0);

        sup.note_hook_started();
        assert_eq!(sup.counters().hooks_started, 1);
        assert_eq!(sup.counters().hooks_finished, 0);
        sup.note_hook_finished();
        assert_eq!(sup.counters().hooks_finished, 1);
    }

    #[test]
    fn a_touching_window_for_the_same_hook_launches_it_again() {
        let mut sup = Supervisor::new(1);
        let mut first = ActiveWindows::new();
        first.insert(0, &ProcessAction::RunHook(2), 0);
        assert_eq!(sup.tick(&first, &[]), [Action::RunHook(2)]);
        sup.note_hook_started();
        let mut second = ActiveWindows::new();
        second.insert(0, &ProcessAction::RunHook(2), 500);
        assert_eq!(sup.tick(&second, &[]), [Action::RunHook(2)]);
        sup.note_hook_started();
        assert_eq!(sup.tick(&second, &[]), []);
        assert_eq!(sup.counters().hooks_started, 2);
    }

    #[test]
    fn actions_are_ordered_by_node_then_hooks_then_restarts() {
        let mut sup = Supervisor::new(3);
        let set = active(&[
            (0, ProcessAction::Pause(1)),
            (1, ProcessAction::Kill),
            (0, ProcessAction::RunHook(3)),
        ]);
        assert_eq!(
            sup.tick(&set, &[2]),
            [
                Action::Stop(0),
                Action::Kill(1),
                Action::RunHook(3),
                Action::Start {
                    node: 2,
                    after_fault: false
                },
            ]
        );
    }

    #[test]
    fn faults_naming_an_unknown_node_are_ignored() {
        let mut sup = Supervisor::new(1);
        let set = active(&[(9, ProcessAction::Kill)]);
        assert_eq!(sup.tick(&set, &[9]), []);
        assert_eq!(sup.alive_bitmap(), 1);
        assert_eq!(sup.counters().unexpected_deaths, 0);
    }

    #[test]
    fn the_snapshot_reports_every_register() {
        let mut sup = Supervisor::new(2);
        sup.tick(&active(&[(1, ProcessAction::RunHook(2))]), &[0]);
        assert_eq!(sup.counters().hooks_started, 0);
        sup.note_hook_started();
        sup.note_hook_finished();
        let snap = sup.snapshot();
        assert_eq!(snap.ticks, 1);
        assert_eq!(snap.alive, 0b11);
        assert_eq!(snap.hooks_started, 1);
        assert_eq!(snap.hooks_finished, 1);
        assert_eq!(snap.unexpected_deaths, 1);
        assert_eq!(snap.restarts, 1);
    }

    #[test]
    fn event_and_lifecycle_counters_only_advance_on_runtime_progress() {
        let mut sup = Supervisor::new(1);
        sup.note_event_parked(2);
        sup.note_workload_started();
        sup.note_workload_finished();
        sup.note_check_started();
        sup.note_check_finished();
        let snap = sup.snapshot();
        assert_eq!(snap.event_park_fires, 2);
        assert_eq!(snap.disturbance_generation, 2);
        assert_eq!(snap.workload_started, 1);
        assert_eq!(snap.workload_finished, 1);
        assert_eq!(snap.checks_started, 1);
        assert_eq!(snap.checks_finished, 1);
    }

    #[test]
    fn held_parks_and_armed_kills_are_published_per_node_without_disturbing() {
        let mut sup = Supervisor::new(3);
        sup.note_event_park_held(2, true);
        sup.note_event_park_held(0, true);
        sup.note_event_park_held(0, false);
        sup.note_event_park_held(64, true);
        sup.note_event_kill_armed(1, 4, 7);
        let snap = sup.snapshot();
        assert_eq!(snap.event_park_held, 0b100);
        assert_eq!(snap.event_kill_armed, 0b010);
        assert_eq!(snap.disturbance_generation, 0);
        assert!(sup.note_event_kill(1, 4, 7, 0xfeed));
        assert_eq!(sup.snapshot().event_kill_armed, 0);
        sup.note_event_kill_armed(2, 5, 9);
        sup.note_event_kill_disarmed(2, 5, 9);
        assert_eq!(sup.snapshot().event_kill_armed, 0);
    }

    #[test]
    fn a_node_that_dies_under_a_pause_window_publishes_no_armed_kill() {
        let mut sup = Supervisor::new(1);
        let kill = active(&[(0, ProcessAction::EventKill { rarity: 0 })]);
        assert_eq!(sup.tick(&kill, &[]), [Action::ArmEventKill(0, 0, 0)]);
        sup.note_event_kill_armed(0, 0, 0);
        assert_eq!(sup.snapshot().event_kill_armed, 1);
        let paused = active(&[
            (0, ProcessAction::EventKill { rarity: 0 }),
            (0, ProcessAction::Pause(30)),
        ]);
        assert_eq!(sup.tick(&paused, &[0]), []);
        assert_eq!(sup.snapshot().alive, 0);
        assert_eq!(sup.snapshot().event_kill_armed, 0);
        assert_eq!(
            sup.tick(&kill, &[]),
            [
                Action::Start {
                    node: 0,
                    after_fault: false
                },
                Action::ArmEventKill(0, 0, 0)
            ]
        );
    }

    #[test]
    fn edge_coverage_sums_across_nodes_without_disturbing() {
        let mut sup = Supervisor::new(2);
        sup.note_edge_coverage(3, u64::MAX);
        sup.note_edge_coverage(2, 2);
        let snap = sup.snapshot();
        assert_eq!(snap.edge_crossings, 5);
        assert_eq!(snap.edge_digest, 1);
        assert_eq!(snap.disturbance_generation, 0);
    }

    #[test]
    fn pending_faults_drop_after_the_canonical_event_report() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, ProcessAction::EventKill { rarity: 0 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 0, 0)]);
        sup.note_event_kill_armed(0, 0, 0);
        assert_eq!(sup.snapshot().pending_faults, 1);
        assert!(sup.note_event_kill(0, 0, 0, 0xfeed));
        assert_eq!(sup.tick(&event, &[]), []);
        assert_eq!(sup.snapshot().pending_faults, 0);
        assert_eq!(sup.snapshot().disturbance_generation, 1);
    }

    #[test]
    fn rejected_event_reports_do_not_advance_disturbance_generation() {
        let mut sup = Supervisor::new(1);
        sup.note_process_transition();
        assert!(!sup.note_event_kill(0, 9, 3, 0xfeed));
        sup.note_event_parked(2);
        assert_eq!(sup.snapshot().disturbance_generation, 3);
    }

    #[test]
    fn stale_pre_fault_check_provenance_remains_distinct_in_the_snapshot() {
        let mut sup = Supervisor::new(1);
        sup.set_check_enabled(true);
        sup.note_process_transition();
        let capture = started(1, &sup);
        sup.note_check_completed(completed(capture, &sup));
        sup.note_process_transition();
        let snap = sup.snapshot();
        assert_eq!(snap.completed_check_start_generation, 1);
        assert_eq!(snap.completed_check_end_generation, 1);
        assert_eq!(snap.completed_check_pid, 7);
        assert_eq!(snap.disturbance_generation, 2);
        assert_ne!(
            snap.completed_check_end_generation,
            snap.disturbance_generation
        );
    }

    #[test]
    fn a_check_spanning_a_disturbance_publishes_both_generations() {
        let mut sup = Supervisor::new(1);
        sup.note_process_transition();
        let capture = started(2, &sup);
        sup.note_process_transition();
        sup.note_check_completed(completed(capture, &sup));
        let snap = sup.snapshot();
        assert_eq!(snap.completed_check_start_generation, 1);
        assert_eq!(snap.completed_check_end_generation, 2);
        assert_eq!(snap.disturbance_generation, 2);
    }

    #[test]
    fn a_late_standing_event_advances_past_completed_check_provenance() {
        let mut sup = Supervisor::new(1);
        sup.note_process_transition();
        let capture = started(3, &sup);
        sup.note_check_completed(completed(capture, &sup));
        sup.note_process_transition();
        let snap = sup.snapshot();
        assert_eq!(snap.completed_check_end_generation, 1);
        assert_eq!(snap.disturbance_generation, 2);
    }

    #[test]
    fn a_check_after_recovery_captures_the_current_generation() {
        let mut sup = Supervisor::new(1);
        sup.note_process_transition();
        sup.note_process_transition();
        let capture = started(4, &sup);
        sup.note_check_completed(completed(capture, &sup));
        let snap = sup.snapshot();
        assert_eq!(snap.completed_check_start_generation, 2);
        assert_eq!(snap.completed_check_end_generation, 2);
        assert_eq!(snap.disturbance_generation, 2);
    }

    #[test]
    fn a_pending_event_arm_excludes_completed_check_provenance() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, ProcessAction::EventKill { rarity: 0 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 0, 0)]);
        sup.note_event_kill_armed(0, 0, 0);
        let capture = started(5, &sup);
        sup.note_check_completed(completed(capture, &sup));
        assert_eq!(sup.snapshot().pending_faults, 1);
        assert!(sup.note_event_kill(0, 0, 0, 0xfeed));
        assert_eq!(sup.tick(&event, &[]), []);
        assert_eq!(sup.snapshot().pending_faults, 0);
    }

    #[test]
    fn infrastructure_error_is_reported_once() {
        let mut sup = Supervisor::new(1);
        assert!(sup.note_infrastructure_error());
        assert!(!sup.note_infrastructure_error());
        assert_eq!(sup.snapshot().infrastructure_error, 1);
    }

    #[test]
    fn disturbance_generation_overflow_is_an_infrastructure_error() {
        let mut sup = Supervisor::new(1);
        sup.counters.disturbance_generation = u64::MAX;
        sup.note_process_transition();
        assert_eq!(sup.snapshot().disturbance_generation, u64::MAX);
        assert_eq!(sup.snapshot().infrastructure_error, 1);
    }

    #[test]
    fn event_ready_is_a_per_node_bitmap() {
        let mut sup = Supervisor::new(3);
        sup.note_event_ready(0, true);
        sup.note_event_ready(2, true);
        assert_eq!(sup.snapshot().event_ready, 0b101);
        sup.note_event_ready(0, false);
        assert_eq!(sup.snapshot().event_ready, 0b100);
        sup.note_event_ready(64, true);
        assert_eq!(sup.snapshot().event_ready, 0b100);
    }

    #[test]
    fn the_serial_log_names_each_action() {
        assert_eq!(Action::Kill(2).describe(), "kill node 2");
        assert_eq!(
            Action::PowerLoss {
                node: 1,
                choice: 0x2a,
                kill: true
            }
            .describe(),
            "power loss node 1 choice 0x2a"
        );
        assert_eq!(
            Action::DiskCorrupt {
                node: 1,
                choice: 0x2a,
                kill: true
            }
            .describe(),
            "corrupt disk node 1 choice 0x2a"
        );
        assert_eq!(
            Action::SetDiskMode {
                node: 2,
                mode: DiskMode::Stall {
                    hold_nanos: 120_000_000
                }
            }
            .describe(),
            "disk node 2 stalls every request for 120 ms"
        );
        assert_eq!(
            Action::SetDiskMode {
                node: 2,
                mode: DiskMode::Error
            }
            .describe(),
            "disk node 2 fails every request"
        );
        assert_eq!(
            Action::SetDiskMode {
                node: 2,
                mode: DiskMode::Normal
            }
            .describe(),
            "disk node 2 serves requests"
        );
        assert_eq!(Action::Stop(0).describe(), "pause node 0");
        assert_eq!(Action::Cont(0).describe(), "resume node 0");
        assert_eq!(
            Action::Start {
                node: 1,
                after_fault: false
            }
            .describe(),
            "start node 1"
        );
        assert_eq!(Action::RunHook(5).describe(), "run hook 5");
        assert_eq!(
            Action::ArmEventKill(0, 3, 0).describe(),
            "arm event kill node 0 rarity 3"
        );
        assert_eq!(
            Action::DisarmEventKill(0).describe(),
            "disarm event kill node 0"
        );
        assert_eq!(
            Action::ArmEventPark(
                0,
                EventPark {
                    edges: 2,
                    hold_nanos: 4,
                    target: None,
                    start: 0,
                }
            )
            .describe(),
            "arm event park node 0 edges 2 hold 4"
        );
        assert_eq!(
            Action::ArmEventPark(
                1,
                EventPark {
                    edges: 1,
                    hold_nanos: 4,
                    target: process_proto::events::ParkTarget::new(0x1000, 0x1004),
                    start: 0,
                }
            )
            .describe(),
            "arm event park node 1 edges 1 hold 4 target 0x1000..0x1004"
        );
        assert_eq!(
            Action::DisarmEventPark(0).describe(),
            "disarm event park node 0"
        );
        assert_eq!(
            Action::SetClock {
                node: 2,
                offset_nanos: -5
            }
            .describe(),
            "shift clock node 2 by -5 ns"
        );
    }

    fn clocked(node: u16, offset_nanos: i64, rate_ppm: i32, moment: u64) -> ActiveWindows {
        let mut set = active(&[(
            node,
            ProcessAction::Clock {
                offset_nanos,
                rate_ppm,
                spread: 0,
            },
        )]);
        set.set_moment(moment);
        set
    }

    #[test]
    fn a_clock_window_shifts_its_node_and_restores_it_on_exit() {
        let mut sup = Supervisor::new(2);
        let generation = sup.disturbance_generation();
        let shifted = clocked(1, 3_000, 0, 10);
        assert_eq!(
            sup.tick(&shifted, &[]),
            [Action::SetClock {
                node: 1,
                offset_nanos: 3_000
            }]
        );
        assert_eq!(sup.disturbance_generation(), generation + 1);
        assert_eq!(sup.pending_faults(), 1);
        assert_eq!(sup.tick(&shifted, &[]), []);
        assert_eq!(sup.clock_offset(1), 3_000);
        assert_eq!(sup.clock_offset(0), 0);
        assert_eq!(
            sup.tick(&ActiveWindows::new(), &[]),
            [Action::SetClock {
                node: 1,
                offset_nanos: 0
            }]
        );
        assert_eq!(sup.disturbance_generation(), generation + 2);
        assert_eq!(sup.pending_faults(), 0);
        assert_eq!(sup.alive_bitmap(), 0b11);
    }

    #[test]
    fn a_clock_rate_moves_the_offset_every_tick() {
        let mut sup = Supervisor::new(1);
        let tick = crate::TICK_NANOS;
        let mut offsets = Vec::new();
        for step in 0..4 {
            for action in sup.tick(&clocked(0, 0, -500_000, step * tick), &[]) {
                let Action::SetClock { offset_nanos, .. } = action else {
                    panic!("a clock window only shifts the clock");
                };
                offsets.push(offset_nanos);
            }
        }
        let half = i64::try_from(tick / 2).unwrap();
        assert_eq!(offsets, [-half, -2 * half, -3 * half]);
    }

    #[test]
    fn a_node_down_during_a_clock_change_starts_with_the_current_offset() {
        let mut sup = Supervisor::new(1);
        let mut down = active(&[
            (0, ProcessAction::Kill),
            (
                0,
                ProcessAction::Clock {
                    offset_nanos: 9,
                    rate_ppm: 0,
                    spread: 0,
                },
            ),
        ]);
        down.set_moment(0);
        assert_eq!(sup.tick(&down, &[]), [Action::Kill(0)]);
        assert_eq!(sup.clock_offset(0), 9);
        let restart = clocked(0, 9, 0, 0);
        let mut restarting = restart.clone();
        restarting.insert(0, &ProcessAction::Restart, 0);
        assert_eq!(sup.tick(&restarting, &[0]), []);
        assert_eq!(
            sup.tick(&restart, &[]),
            [Action::Start {
                node: 0,
                after_fault: true
            }]
        );
        assert_eq!(sup.clock_offset(0), 9);
    }
}
