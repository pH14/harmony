// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::reconcile::{ActiveWindows, Park};
use crate::regs::RegisterSnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Kill(u16),
    Stop(u16),
    Cont(u16),
    Start(u16),
    RunHook(u32),
    Park(u16, Park),
    Unpark(u16),
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
            Action::Park(node, park) => format!(
                "park node {node} at {:#x} hit {} hold {}",
                park.addr, park.hits, park.hold_nanos
            ),
            Action::Unpark(node) => format!("unpark node {node}"),
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
    pub sometimes: u64,
    pub parked: u64,
}

#[derive(Clone, Copy, Debug)]
struct NodeState {
    alive: bool,
    paused: bool,
    expected_down: bool,
}

#[derive(Clone, Debug)]
pub struct ProcessSupervisor {
    nodes: Vec<NodeState>,
    previous: ActiveWindows,
    counters: Counters,
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
                };
                node_count
            ],
            previous: ActiveWindows::new(),
            counters: Counters::default(),
        }
    }

    pub fn tick(&mut self, active: &ActiveWindows, deaths: &[u16]) -> Vec<Action> {
        self.counters.ticks += 1;
        for &node in deaths {
            let Some(state) = self.nodes.get_mut(usize::from(node)) else {
                continue;
            };
            if !state.alive {
                continue;
            }
            state.alive = false;
            state.paused = false;
            if !state.expected_down {
                self.counters.unexpected_deaths += 1;
            }
        }

        let mut actions = Vec::new();
        for index in 0..self.nodes.len() {
            let node = index as u16;
            let was = self.previous.node(node);
            let now = active.node(node);
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
            if let Some(park) = now.park {
                if was.park != Some(park) {
                    actions.push(Action::Park(node, park));
                }
            } else if was.park.is_some() {
                actions.push(Action::Unpark(node));
            }
            if !now.restart && was.restart && !now.kill && !state.alive {
                actions.push(Action::Start(node));
                state.alive = true;
                state.paused = false;
                state.expected_down = false;
                self.counters.restarts += 1;
            }
        }

        for window in active.hooks() {
            if self.previous.hooks().binary_search(window).is_err() {
                actions.push(Action::RunHook(window.id));
                self.counters.hooks_started += 1;
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
            self.counters.restarts += 1;
        }

        self.previous = active.clone();
        actions
    }

    pub fn note_hook_finished(&mut self) {
        self.counters.hooks_finished += 1;
    }

    pub fn note_parked(&mut self) {
        self.counters.parked += 1;
    }

    pub fn note_sometimes(&mut self, id: u32) {
        if let Some(bit) = crate::regs::sometimes_bit(id) {
            self.counters.sometimes |= bit;
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
            sometimes: self.counters.sometimes,
            unexpected_deaths: self.counters.unexpected_deaths,
            restarts: self.counters.restarts,
            parked: self.counters.parked,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::ActiveWindows;
    use process_proto::ProcessAction;

    fn active(actions: &[(u16, ProcessAction)]) -> ActiveWindows {
        let mut set = ActiveWindows::new();
        for (node, action) in actions {
            set.insert(*node, action, 0);
        }
        set
    }

    #[test]
    fn a_kill_window_kills_once_and_the_node_stays_down() {
        let mut supervisor = Supervisor::new(2);
        let killed = active(&[(0, ProcessAction::Kill)]);
        assert_eq!(supervisor.tick(&killed, &[]), [Action::Kill(0)]);
        assert_eq!(supervisor.alive_bitmap(), 0b10);
        assert_eq!(supervisor.tick(&killed, &[]), []);
        assert_eq!(supervisor.tick(&killed, &[0]), []);
        assert_eq!(supervisor.tick(&ActiveWindows::new(), &[]), []);
        assert_eq!(supervisor.tick(&ActiveWindows::new(), &[]), []);
        assert_eq!(supervisor.alive_bitmap(), 0b10);
        assert_eq!(supervisor.counters().unexpected_deaths, 0);
        assert_eq!(supervisor.counters().restarts, 0);
        assert_eq!(supervisor.counters().ticks, 5);
    }

    #[test]
    fn a_pause_window_stops_on_entry_and_continues_on_exit() {
        let mut supervisor = Supervisor::new(1);
        let paused = active(&[(0, ProcessAction::Pause(30))]);
        assert_eq!(supervisor.tick(&paused, &[]), [Action::Stop(0)]);
        assert_eq!(supervisor.tick(&paused, &[]), []);
        assert_eq!(
            supervisor.tick(&ActiveWindows::new(), &[]),
            [Action::Cont(0)]
        );
        assert_eq!(supervisor.tick(&ActiveWindows::new(), &[]), []);
        assert_eq!(supervisor.alive_bitmap(), 1);
        assert_eq!(supervisor.counters().restarts, 0);
    }

    #[test]
    fn a_restart_window_kills_on_entry_and_starts_on_exit() {
        let mut supervisor = Supervisor::new(1);
        let restart = active(&[(0, ProcessAction::Restart)]);
        assert_eq!(supervisor.tick(&restart, &[]), [Action::Kill(0)]);
        assert_eq!(supervisor.alive_bitmap(), 0);
        assert_eq!(supervisor.tick(&restart, &[0]), []);
        assert_eq!(
            supervisor.tick(&ActiveWindows::new(), &[]),
            [Action::Start(0)]
        );
        assert_eq!(supervisor.alive_bitmap(), 1);
        assert_eq!(supervisor.counters().restarts, 1);
        assert_eq!(supervisor.counters().unexpected_deaths, 0);
    }

    #[test]
    fn a_restart_closing_under_a_kill_leaves_the_node_down() {
        let mut supervisor = Supervisor::new(1);
        let both = active(&[(0, ProcessAction::Restart), (0, ProcessAction::Kill)]);
        assert_eq!(supervisor.tick(&both, &[]), [Action::Kill(0)]);
        let kill = active(&[(0, ProcessAction::Kill)]);
        assert_eq!(supervisor.tick(&kill, &[]), []);
        assert_eq!(supervisor.alive_bitmap(), 0);
        assert_eq!(supervisor.counters().restarts, 0);
    }

    #[test]
    fn a_pause_leaving_with_the_node_dead_starts_it() {
        let mut supervisor = Supervisor::new(1);
        let paused = active(&[(0, ProcessAction::Pause(1))]);
        assert_eq!(supervisor.tick(&paused, &[]), [Action::Stop(0)]);
        assert_eq!(supervisor.tick(&paused, &[0]), []);
        assert_eq!(
            supervisor.tick(&ActiveWindows::new(), &[]),
            [Action::Start(0)]
        );
        assert_eq!(supervisor.counters().unexpected_deaths, 1);
        assert_eq!(supervisor.counters().restarts, 1);
    }

    #[test]
    fn an_unexpected_death_is_counted_and_restarted_next_tick() {
        let mut supervisor = Supervisor::new(2);
        assert_eq!(
            supervisor.tick(&ActiveWindows::new(), &[1]),
            [Action::Start(1)]
        );
        assert_eq!(supervisor.alive_bitmap(), 0b11);
        assert_eq!(supervisor.counters().unexpected_deaths, 1);
        assert_eq!(supervisor.counters().restarts, 1);
        assert_eq!(
            supervisor.tick(&ActiveWindows::new(), &[1, 1]),
            [Action::Start(1)]
        );
        assert_eq!(supervisor.counters().unexpected_deaths, 2);
    }

    #[test]
    fn a_death_under_a_pause_window_waits_until_the_window_closes() {
        let mut supervisor = Supervisor::new(1);
        let paused = active(&[(0, ProcessAction::Pause(1))]);
        supervisor.tick(&paused, &[]);
        assert_eq!(supervisor.tick(&paused, &[0]), []);
        assert_eq!(supervisor.tick(&paused, &[]), []);
        assert_eq!(
            supervisor.tick(&ActiveWindows::new(), &[]),
            [Action::Start(0)]
        );
    }

    #[test]
    fn a_park_window_arms_on_entry_and_disarms_on_exit() {
        let mut supervisor = Supervisor::new(1);
        let park = Park {
            addr: 0x4b0e86,
            hits: 28,
            hold_nanos: 2_000_000,
        };
        let parked = active(&[(
            0,
            ProcessAction::Park {
                addr: 0x4b0e86,
                hits: 28,
                hold_nanos: 2_000_000,
            },
        )]);
        assert_eq!(supervisor.tick(&parked, &[]), [Action::Park(0, park)]);
        assert_eq!(supervisor.tick(&parked, &[]), []);
        assert_eq!(
            supervisor.tick(&ActiveWindows::new(), &[]),
            [Action::Unpark(0)]
        );
        assert_eq!(supervisor.tick(&ActiveWindows::new(), &[]), []);
        supervisor.note_parked();
        assert_eq!(supervisor.snapshot().parked, 1);
        assert_eq!(supervisor.alive_bitmap(), 1);
    }

    #[test]
    fn hooks_launch_once_per_window_and_ids_are_ascending() {
        let mut supervisor = Supervisor::new(1);
        let two = active(&[
            (0, ProcessAction::RunHook(4)),
            (0, ProcessAction::RunHook(1)),
        ]);
        assert_eq!(
            supervisor.tick(&two, &[]),
            [Action::RunHook(1), Action::RunHook(4)]
        );
        assert_eq!(supervisor.tick(&two, &[]), []);
        let next = active(&[
            (0, ProcessAction::RunHook(4)),
            (0, ProcessAction::RunHook(7)),
        ]);
        assert_eq!(supervisor.tick(&next, &[]), [Action::RunHook(7)]);
        assert_eq!(supervisor.tick(&two, &[]), [Action::RunHook(1)]);
        assert_eq!(supervisor.counters().hooks_started, 4);
    }

    #[test]
    fn a_touching_window_for_the_same_hook_launches_it_again() {
        let mut supervisor = Supervisor::new(1);
        let mut first = ActiveWindows::new();
        first.insert(0, &ProcessAction::RunHook(2), 0);
        assert_eq!(supervisor.tick(&first, &[]), [Action::RunHook(2)]);
        let mut second = ActiveWindows::new();
        second.insert(0, &ProcessAction::RunHook(2), 500);
        assert_eq!(supervisor.tick(&second, &[]), [Action::RunHook(2)]);
        assert_eq!(supervisor.tick(&second, &[]), []);
        assert_eq!(supervisor.counters().hooks_started, 2);
    }

    #[test]
    fn actions_are_ordered_by_node_then_hooks_then_restarts() {
        let mut supervisor = Supervisor::new(3);
        let set = active(&[
            (0, ProcessAction::Pause(1)),
            (1, ProcessAction::Kill),
            (0, ProcessAction::RunHook(3)),
        ]);
        assert_eq!(
            supervisor.tick(&set, &[2]),
            [
                Action::Stop(0),
                Action::Kill(1),
                Action::RunHook(3),
                Action::Start(2)
            ]
        );
    }

    #[test]
    fn actions_naming_an_unknown_node_are_ignored() {
        let mut supervisor = Supervisor::new(1);
        let set = active(&[(9, ProcessAction::Kill)]);
        assert_eq!(supervisor.tick(&set, &[9]), []);
        assert_eq!(supervisor.alive_bitmap(), 1);
        assert_eq!(supervisor.counters().unexpected_deaths, 0);
    }

    #[test]
    fn the_sometimes_bitmap_covers_the_first_forty_eight_ids() {
        let mut supervisor = Supervisor::new(1);
        supervisor.note_sometimes(0);
        supervisor.note_sometimes(47);
        supervisor.note_sometimes(48);
        supervisor.note_sometimes(u32::MAX);
        assert_eq!(supervisor.counters().sometimes, 1 | (1 << 47));
    }

    #[test]
    fn the_snapshot_reports_every_register() {
        let mut supervisor = Supervisor::new(2);
        supervisor.tick(&active(&[(1, ProcessAction::RunHook(2))]), &[0]);
        supervisor.note_hook_finished();
        supervisor.note_sometimes(3);
        let snapshot = supervisor.snapshot();
        assert_eq!(snapshot.ticks, 1);
        assert_eq!(snapshot.alive, 0b11);
        assert_eq!(snapshot.hooks_started, 1);
        assert_eq!(snapshot.hooks_finished, 1);
        assert_eq!(snapshot.sometimes, 1 << 3);
        assert_eq!(snapshot.unexpected_deaths, 1);
        assert_eq!(snapshot.restarts, 1);
    }

    #[test]
    fn the_serial_log_names_each_action() {
        assert_eq!(Action::Kill(2).describe(), "kill node 2");
        assert_eq!(Action::Stop(0).describe(), "pause node 0");
        assert_eq!(Action::Cont(0).describe(), "resume node 0");
        assert_eq!(Action::Start(1).describe(), "start node 1");
        assert_eq!(Action::RunHook(5).describe(), "run hook 5");
        assert_eq!(
            Action::Park(
                0,
                Park {
                    addr: 0x4b0e86,
                    hits: 28,
                    hold_nanos: 2_000_000,
                },
            )
            .describe(),
            "park node 0 at 0x4b0e86 hit 28 hold 2000000"
        );
        assert_eq!(Action::Unpark(0).describe(), "unpark node 0");
    }
}
