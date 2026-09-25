// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::evidence::CheckEvidence;
use crate::reconcile::{ActiveWindows, EventKillWindow, EventPark};
use crate::regs::RegisterSnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Kill(u16),
    Stop(u16),
    Cont(u16),
    Start(u16),
    RunHook(u32),
    ArmEventKill(u16, u8),
    DisarmEventKill(u16),
    ArmEventPark(u16, EventPark),
    DisarmEventPark(u16),
}

impl Action {
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Action::Kill(node) => format!("kill node {node}"),
            Action::Stop(node) => format!("pause node {node}"),
            Action::Cont(node) => format!("resume node {node}"),
            Action::Start(node) => format!("start node {node}"),
            Action::RunHook(id) => format!("run hook {id}"),
            Action::ArmEventKill(node, rarity) => {
                format!("arm event kill node {node} rarity {rarity}")
            }
            Action::DisarmEventKill(node) => format!("disarm event kill node {node}"),
            Action::ArmEventPark(node, park) => format!(
                "arm event park node {node} edges {} hold {}",
                park.edges, park.hold_nanos
            ),
            Action::DisarmEventPark(node) => format!("disarm event park node {node}"),
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
                };
                node_count
            ],
            previous: ActiveWindows::new(),
            counters: Counters::default(),
            event_kill_selected: vec![None; node_count],
            event_kill_armed: Vec::new(),
            event_kill_fired: Vec::new(),
            natural_deaths: Vec::new(),
        }
    }

    pub fn take_natural_deaths(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.natural_deaths)
    }

    pub fn tick(&mut self, active: &ActiveWindows, deaths: &[u16]) -> Vec<Action> {
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
                let natural = !state.expected_down && !event_kill_death;
                state.alive = false;
                state.paused = false;
                natural
            };
            if natural {
                self.bump_disturbance(1);
                self.counters.unexpected_deaths += 1;
                self.natural_deaths.push(node);
            }
        }

        let mut actions = Vec::new();
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
                }
                state.expected_down = true;
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
                            actions.push(Action::ArmEventKill(node, window.rarity));
                        }
                        (Some(_), None) if !was_event_kill_fired => {
                            actions.push(Action::DisarmEventKill(node));
                        }
                        (Some(previous), Some(window)) if previous != window => {
                            if !was_event_kill_fired {
                                actions.push(Action::DisarmEventKill(node));
                            }
                            actions.push(Action::ArmEventKill(node, window.rarity));
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
            if !now.restart && was.restart && !now.kill && !state.alive {
                actions.push(Action::Start(node));
                state.alive = true;
                state.paused = false;
                state.expected_down = false;
                state.event_kill_death = false;
                self.counters.restarts += 1;
                if let Some(window) = now_event_kill {
                    actions.push(Action::ArmEventKill(node, window.rarity));
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
            actions.push(Action::Start(node));
            self.nodes[index].alive = true;
            self.nodes[index].paused = false;
            self.nodes[index].event_kill_death = false;
            self.counters.restarts += 1;
            if let Some(window) = self.event_kill_selected[index] {
                actions.push(Action::ArmEventKill(node, window.rarity));
            }
            if let Some(park) = active.node(node).event_park {
                actions.push(Action::ArmEventPark(node, park));
            }
        }

        self.counters.pending_faults = active.pending_process_faults(&self.event_kill_fired);
        self.previous = active.clone();
        actions
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

    #[must_use]
    pub fn event_kill_start(&self, node: u16, rarity: u8) -> Option<u64> {
        self.event_kill_selected
            .get(usize::from(node))
            .copied()
            .flatten()
            .filter(|window| window.rarity == rarity)
            .map(|window| window.start)
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
    use process_proto::ProcessAction;

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
        assert_eq!(sup.counters().unexpected_deaths, 0);
        assert_eq!(sup.counters().restarts, 0);
        assert_eq!(sup.counters().ticks, 5);
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
        assert_eq!(sup.tick(&ActiveWindows::new(), &[]), [Action::Start(0)]);
        assert_eq!(sup.alive_bitmap(), 1);
        assert_eq!(sup.counters().restarts, 1);
        assert_eq!(sup.counters().unexpected_deaths, 0);
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
        assert_eq!(sup.tick(&ActiveWindows::new(), &[]), [Action::Start(0)]);
        assert_eq!(sup.counters().unexpected_deaths, 1);
        assert_eq!(sup.counters().restarts, 1);
    }

    #[test]
    fn an_unexpected_death_is_counted_and_restarted_next_tick() {
        let mut sup = Supervisor::new(2);
        assert_eq!(sup.tick(&ActiveWindows::new(), &[1]), [Action::Start(1)]);
        assert_eq!(sup.alive_bitmap(), 0b11);
        assert_eq!(sup.counters().unexpected_deaths, 1);
        assert_eq!(sup.counters().restarts, 1);
        assert_eq!(sup.tick(&ActiveWindows::new(), &[1, 1]), [Action::Start(1)]);
        assert_eq!(sup.counters().unexpected_deaths, 2);
    }

    #[test]
    fn a_death_under_a_pause_window_is_not_restarted_until_the_window_closes() {
        let mut sup = Supervisor::new(1);
        let paused = active(&[(0, ProcessAction::Pause(1))]);
        sup.tick(&paused, &[]);
        assert_eq!(sup.tick(&paused, &[0]), []);
        assert_eq!(sup.tick(&paused, &[]), []);
        assert_eq!(sup.tick(&ActiveWindows::new(), &[]), [Action::Start(0)]);
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
                },
            ),
        ]);
        assert_eq!(
            sup.tick(&event, &[]),
            [
                Action::ArmEventKill(0, 2),
                Action::ArmEventPark(
                    0,
                    EventPark {
                        edges: 3,
                        hold_nanos: 8,
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
            },
            0,
        );
        let mut second = ActiveWindows::new();
        second.insert(
            0,
            &ProcessAction::EventPark {
                edges: 3,
                hold_nanos: 10_000_000,
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
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 0)]);
        assert_eq!(
            sup.tick(&event, &[0]),
            [Action::Start(0), Action::ArmEventKill(0, 0)]
        );
        assert_eq!(sup.counters().unexpected_deaths, 1);

        let mut sup = Supervisor::new(1);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 0)]);
        sup.note_event_kill_armed(0, 0, 0);
        assert!(sup.note_event_kill(0, 0, 0, 0xfeed));
        assert_eq!(sup.tick(&event, &[0]), [Action::Start(0)]);
        assert_eq!(sup.counters().unexpected_deaths, 0);
        assert_eq!(sup.counters().event_kill_fires, 1);
        assert_eq!(sup.counters().event_kill_site, 0xfeed);
    }

    #[test]
    fn an_event_kill_report_survives_until_the_death_is_observed() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, ProcessAction::EventKill { rarity: 0 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 0)]);
        sup.note_event_kill_armed(0, 0, 0);
        assert!(sup.note_event_kill(0, 0, 0, 0xfeed));
        assert_eq!(sup.tick(&event, &[]), []);
        assert_eq!(sup.tick(&event, &[0]), [Action::Start(0)]);
        assert_eq!(sup.counters().unexpected_deaths, 0);
    }

    #[test]
    fn an_event_kill_report_does_not_cross_an_incarnation_replacement() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, ProcessAction::EventKill { rarity: 0 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 0)]);
        sup.note_event_kill_armed(0, 0, 0);
        assert!(sup.note_event_kill(0, 0, 0, 0xfeed));
        let restart = active(&[(0, ProcessAction::Restart)]);
        assert_eq!(sup.tick(&restart, &[]), [Action::Kill(0)]);
        assert_eq!(sup.tick(&ActiveWindows::new(), &[]), [Action::Start(0)]);
        assert_eq!(sup.tick(&ActiveWindows::new(), &[0]), [Action::Start(0)]);
        assert_eq!(sup.counters().unexpected_deaths, 1);
    }

    #[test]
    fn a_fired_event_kill_advances_to_the_next_window() {
        let mut sup = Supervisor::new(1);
        let mut events = ActiveWindows::new();
        events.insert(0, &ProcessAction::EventKill { rarity: 0 }, 0);
        events.insert(0, &ProcessAction::EventKill { rarity: 0 }, 500_000_000);

        assert_eq!(sup.tick(&events, &[]), [Action::ArmEventKill(0, 0)]);
        sup.note_event_kill_armed(0, 0, 0);
        assert!(sup.note_event_kill(0, 0, 0, 0xfeed));
        assert_eq!(
            sup.tick(&events, &[0]),
            [Action::Start(0), Action::ArmEventKill(0, 0)]
        );
        assert_eq!(sup.event_kill_start(0, 0), Some(500_000_000));
        assert_eq!(sup.pending_faults(), 1);

        sup.note_event_kill_armed(0, 0, 500_000_000);
        assert!(sup.note_event_kill(0, 0, 500_000_000, 0xcafe));
        assert_eq!(sup.tick(&events, &[0]), [Action::Start(0)]);
        assert_eq!(sup.pending_faults(), 0);
        assert_eq!(sup.counters().event_kill_fires, 2);
    }

    #[test]
    fn restarting_a_node_rearms_an_event_fault_that_stayed_active() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, ProcessAction::EventKill { rarity: 1 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 1)]);
        assert_eq!(
            sup.tick(&event, &[0]),
            [Action::Start(0), Action::ArmEventKill(0, 1)]
        );
    }

    #[test]
    fn a_report_for_an_obsolete_arm_does_not_credit_the_current_incarnation() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, ProcessAction::EventKill { rarity: 4 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 4)]);
        assert!(!sup.note_event_kill(0, 4, 9, 0x10));
        assert_eq!(
            sup.tick(&event, &[0]),
            [Action::Start(0), Action::ArmEventKill(0, 4)]
        );
        assert_eq!(sup.counters().event_kill_fires, 0);
        assert_eq!(sup.counters().unexpected_deaths, 1);
        assert_eq!(sup.counters().disturbance_generation, 1);
    }

    #[test]
    fn an_acknowledged_old_window_is_valid_after_the_current_window_changes() {
        let mut sup = Supervisor::new(1);
        let first = active(&[(0, ProcessAction::EventKill { rarity: 4 })]);
        assert_eq!(sup.tick(&first, &[]), [Action::ArmEventKill(0, 4)]);
        sup.note_event_kill_armed(0, 4, 0);

        let mut second = ActiveWindows::new();
        second.insert(0, &ProcessAction::EventKill { rarity: 4 }, 1);
        assert_eq!(
            sup.tick(&second, &[]),
            [Action::DisarmEventKill(0), Action::ArmEventKill(0, 4)]
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
                Action::Start(2),
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
        assert_eq!(sup.tick(&kill, &[]), [Action::ArmEventKill(0, 0)]);
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
            [Action::Start(0), Action::ArmEventKill(0, 0)]
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
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 0)]);
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
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 0)]);
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
        assert_eq!(Action::Stop(0).describe(), "pause node 0");
        assert_eq!(Action::Cont(0).describe(), "resume node 0");
        assert_eq!(Action::Start(1).describe(), "start node 1");
        assert_eq!(Action::RunHook(5).describe(), "run hook 5");
        assert_eq!(
            Action::ArmEventKill(0, 3).describe(),
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
                    start: 0,
                }
            )
            .describe(),
            "arm event park node 0 edges 2 hold 4"
        );
        assert_eq!(
            Action::DisarmEventPark(0).describe(),
            "disarm event park node 0"
        );
    }
}
