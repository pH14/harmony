// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reconciliation between two consecutive standing-poll answers.
//!
//! The host's answer is a *state*, not a command stream: it says which fault
//! windows contain the current `Moment`. The agent turns state into edges by
//! diffing each answer against the previous one, so a fault applies once when
//! its window opens and is undone once when the window closes.
//!
//! | fault | window opens | window closes |
//! |---|---|---|
//! | `ProcKill` | `SIGKILL` the node's group | nothing: a kill is permanent |
//! | `ProcPause` | `SIGSTOP` | `SIGCONT` |
//! | `ProcRestart` | `SIGKILL` | start the node again |
//! | `RunHook` | launch the hook once | nothing: hooks are not awaited |
//! | `ProcPark` | arm the park on the node's process group | disarm it; a hold in progress finishes |
//!
//! A node that exits while no fault names it is an unexpected death: it is
//! counted and started again at the next tick, so a workload that crashes on
//! its own keeps running and the count is the observable.

use crate::faults::{ActiveFaults, Park};
use crate::regs::RegisterSnapshot;

/// One thing the agent must do to the guest this tick, in the order the
/// supervisor emits them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    /// `SIGKILL` the node's process group.
    Kill(u16),
    /// `SIGSTOP` the node's process group.
    Stop(u16),
    /// `SIGCONT` the node's process group.
    Cont(u16),
    /// Spawn the node's command in a fresh process group.
    Start(u16),
    /// Launch the hook once, without waiting for it.
    RunHook(u32),
    /// Arm a park on the node's process group through the guest kernel.
    Park(u16, Park),
    /// Disarm the node's park; a hold already taken runs to its end.
    Unpark(u16),
}

impl Action {
    /// The serial-log description of this action, the `<what>` of a
    /// `FA: <tick> <what>` line.
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

/// The counters the agent publishes as IJON state registers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counters {
    /// Completed poll ticks.
    pub ticks: u64,
    /// Hooks launched.
    pub hooks_started: u64,
    /// Hooks that have exited.
    pub hooks_finished: u64,
    /// Node exits with no fault in force.
    pub unexpected_deaths: u64,
    /// Node starts after the initial one.
    pub restarts: u64,
    /// Bitmap of the `assert_sometimes` ids a hook has reported.
    pub sometimes: u64,
    /// Threads the guest kernel has parked at a place.
    pub parked: u64,
}

#[derive(Clone, Copy, Debug)]
struct NodeState {
    alive: bool,
    paused: bool,
    /// Set while the node is down because the agent killed it. It suppresses
    /// both the unexpected-death count and the automatic restart, which is what
    /// makes `ProcKill` permanent and `ProcRestart` the only fault that brings
    /// a node back.
    expected_down: bool,
}

/// The agent's model of its nodes and the last answer it applied.
#[derive(Clone, Debug)]
pub struct Supervisor {
    nodes: Vec<NodeState>,
    previous: ActiveFaults,
    counters: Counters,
}

impl Supervisor {
    /// A supervisor for `node_count` nodes the caller has already started.
    #[must_use]
    pub fn new(node_count: usize) -> Self {
        Self {
            nodes: vec![
                NodeState {
                    alive: true,
                    paused: false,
                    expected_down: false,
                };
                node_count
            ],
            previous: ActiveFaults::new(),
            counters: Counters::default(),
        }
    }

    /// Reconcile one standing-poll answer, given the nodes observed to have
    /// exited since the last tick, and return the actions to apply in order.
    pub fn tick(&mut self, active: &ActiveFaults, deaths: &[u16]) -> Vec<Action> {
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
            // The node count is capped at `bundle::MAX_NODES`, so the index
            // always fits a u16 node id.
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
            // A restart window closing brings the node back unless a kill still
            // names it.
            if !now.restart && was.restart && !now.kill && !state.alive {
                actions.push(Action::Start(node));
                state.alive = true;
                state.paused = false;
                state.expected_down = false;
                self.counters.restarts += 1;
            }
        }

        for &id in active.hooks() {
            if self.previous.hooks().binary_search(&id).is_err() {
                actions.push(Action::RunHook(id));
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

    /// Record that a launched hook has exited.
    pub fn note_hook_finished(&mut self) {
        self.counters.hooks_finished += 1;
    }

    /// Record that a park took its hit and held a thread.
    pub fn note_parked(&mut self) {
        self.counters.parked += 1;
    }

    /// Record an `assert_sometimes` hit reported by a hook. Ids at or beyond
    /// [`regs::SOMETIMES_BITMAP_IDS`](crate::regs::SOMETIMES_BITMAP_IDS) are
    /// still forwarded to the host by the caller; only the bitmap register is
    /// this narrow.
    pub fn note_sometimes(&mut self, id: u32) {
        if let Some(bit) = crate::regs::sometimes_bit(id) {
            self.counters.sometimes |= bit;
        }
    }

    /// One bit per node, set while the node is running.
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

    /// The counters as published.
    #[must_use]
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// The current register values.
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
    use fault_policy::{Fault, Span};

    fn active(faults: &[(u16, Fault)]) -> ActiveFaults {
        let mut set = ActiveFaults::new();
        for (node, fault) in faults {
            set.insert(*node, fault);
        }
        set
    }

    #[test]
    fn a_kill_window_kills_once_and_the_node_stays_down() {
        let mut sup = Supervisor::new(2);
        let killed = active(&[(0, Fault::ProcKill)]);
        assert_eq!(sup.tick(&killed, &[]), [Action::Kill(0)]);
        assert_eq!(sup.alive_bitmap(), 0b10);
        // The window persists: no repeat signal.
        assert_eq!(sup.tick(&killed, &[]), []);
        // The kill's exit is reaped; it is not an unexpected death.
        assert_eq!(sup.tick(&killed, &[0]), []);
        // The window closes: a kill is permanent, so nothing restarts it.
        assert_eq!(sup.tick(&ActiveFaults::new(), &[]), []);
        assert_eq!(sup.tick(&ActiveFaults::new(), &[]), []);
        assert_eq!(sup.alive_bitmap(), 0b10);
        assert_eq!(sup.counters().unexpected_deaths, 0);
        assert_eq!(sup.counters().restarts, 0);
        assert_eq!(sup.counters().ticks, 5);
    }

    #[test]
    fn a_pause_window_stops_on_entry_and_continues_on_exit() {
        let mut sup = Supervisor::new(1);
        let paused = active(&[(0, Fault::ProcPause(Span(30)))]);
        assert_eq!(sup.tick(&paused, &[]), [Action::Stop(0)]);
        assert_eq!(sup.tick(&paused, &[]), []);
        assert_eq!(sup.tick(&ActiveFaults::new(), &[]), [Action::Cont(0)]);
        assert_eq!(sup.tick(&ActiveFaults::new(), &[]), []);
        // A paused node is still alive, and pausing never counts a restart.
        assert_eq!(sup.alive_bitmap(), 1);
        assert_eq!(sup.counters().restarts, 0);
    }

    #[test]
    fn a_restart_window_kills_on_entry_and_starts_on_exit() {
        let mut sup = Supervisor::new(1);
        let restart = active(&[(0, Fault::ProcRestart)]);
        assert_eq!(sup.tick(&restart, &[]), [Action::Kill(0)]);
        assert_eq!(sup.alive_bitmap(), 0);
        // The exit arrives while the window is open: expected, not counted.
        assert_eq!(sup.tick(&restart, &[0]), []);
        assert_eq!(sup.tick(&ActiveFaults::new(), &[]), [Action::Start(0)]);
        assert_eq!(sup.alive_bitmap(), 1);
        assert_eq!(sup.counters().restarts, 1);
        assert_eq!(sup.counters().unexpected_deaths, 0);
    }

    #[test]
    fn a_restart_closing_under_a_kill_leaves_the_node_down() {
        let mut sup = Supervisor::new(1);
        let both = active(&[(0, Fault::ProcRestart), (0, Fault::ProcKill)]);
        assert_eq!(sup.tick(&both, &[]), [Action::Kill(0)]);
        // Only the restart window closes.
        let kill = active(&[(0, Fault::ProcKill)]);
        assert_eq!(sup.tick(&kill, &[]), []);
        assert_eq!(sup.alive_bitmap(), 0);
        assert_eq!(sup.counters().restarts, 0);
    }

    #[test]
    fn a_pause_leaving_with_the_node_dead_emits_no_signal() {
        let mut sup = Supervisor::new(1);
        let paused = active(&[(0, Fault::ProcPause(Span(1)))]);
        assert_eq!(sup.tick(&paused, &[]), [Action::Stop(0)]);
        // The node dies while stopped: no SIGCONT to a corpse, and the death
        // is unexpected because no kill named it.
        assert_eq!(sup.tick(&paused, &[0]), []);
        assert_eq!(sup.tick(&ActiveFaults::new(), &[]), [Action::Start(0)]);
        assert_eq!(sup.counters().unexpected_deaths, 1);
        assert_eq!(sup.counters().restarts, 1);
    }

    #[test]
    fn an_unexpected_death_is_counted_and_restarted_next_tick() {
        let mut sup = Supervisor::new(2);
        assert_eq!(sup.tick(&ActiveFaults::new(), &[1]), [Action::Start(1)]);
        assert_eq!(sup.alive_bitmap(), 0b11);
        assert_eq!(sup.counters().unexpected_deaths, 1);
        assert_eq!(sup.counters().restarts, 1);
        // A repeated death report for a node already restarted is not counted
        // twice unless the node really exited again.
        assert_eq!(sup.tick(&ActiveFaults::new(), &[1, 1]), [Action::Start(1)]);
        assert_eq!(sup.counters().unexpected_deaths, 2);
    }

    #[test]
    fn a_death_under_a_pause_window_is_not_restarted_until_the_window_closes() {
        let mut sup = Supervisor::new(1);
        let paused = active(&[(0, Fault::ProcPause(Span(1)))]);
        sup.tick(&paused, &[]);
        // While any fault names the node the agent leaves it alone.
        assert_eq!(sup.tick(&paused, &[0]), []);
        assert_eq!(sup.tick(&paused, &[]), []);
        assert_eq!(sup.tick(&ActiveFaults::new(), &[]), [Action::Start(0)]);
    }

    #[test]
    fn a_park_window_arms_on_entry_and_disarms_on_exit() {
        let mut sup = Supervisor::new(1);
        let park = Park {
            addr: 0x4b0e86,
            hits: 28,
            hold_nanos: 2_000_000,
        };
        let parked = active(&[(
            0,
            Fault::ProcPark {
                addr: 0x4b0e86,
                hits: 28,
                hold: Span(2_000_000),
            },
        )]);
        assert_eq!(sup.tick(&parked, &[]), [Action::Park(0, park)]);
        assert_eq!(sup.tick(&parked, &[]), []);
        assert_eq!(sup.tick(&ActiveFaults::new(), &[]), [Action::Unpark(0)]);
        assert_eq!(sup.tick(&ActiveFaults::new(), &[]), []);
        sup.note_parked();
        assert_eq!(sup.snapshot().parked, 1);
        assert_eq!(sup.alive_bitmap(), 1);
    }

    #[test]
    fn hooks_launch_once_per_window_and_ids_are_ascending() {
        let mut sup = Supervisor::new(1);
        let two = active(&[(0, Fault::RunHook(4)), (0, Fault::RunHook(1))]);
        assert_eq!(
            sup.tick(&two, &[]),
            [Action::RunHook(1), Action::RunHook(4)]
        );
        assert_eq!(sup.tick(&two, &[]), []);
        // One window closes and a new one opens in the same tick.
        let next = active(&[(0, Fault::RunHook(4)), (0, Fault::RunHook(7))]);
        assert_eq!(sup.tick(&next, &[]), [Action::RunHook(7)]);
        // Reopening a closed window launches it again.
        assert_eq!(sup.tick(&two, &[]), [Action::RunHook(1)]);
        assert_eq!(sup.counters().hooks_started, 4);
    }

    #[test]
    fn actions_are_ordered_by_node_then_hooks_then_restarts() {
        let mut sup = Supervisor::new(3);
        let set = active(&[
            (0, Fault::ProcPause(Span(1))),
            (1, Fault::ProcKill),
            (0, Fault::RunHook(3)),
        ]);
        // Node 2 died with nothing naming it, so its restart trails the rest.
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
        let set = active(&[(9, Fault::ProcKill)]);
        assert_eq!(sup.tick(&set, &[9]), []);
        assert_eq!(sup.alive_bitmap(), 1);
        assert_eq!(sup.counters().unexpected_deaths, 0);
    }

    #[test]
    fn the_sometimes_bitmap_covers_the_first_forty_eight_ids() {
        let mut sup = Supervisor::new(1);
        sup.note_sometimes(0);
        sup.note_sometimes(47);
        sup.note_sometimes(48);
        sup.note_sometimes(u32::MAX);
        assert_eq!(sup.counters().sometimes, 1 | (1 << 47));
    }

    #[test]
    fn the_snapshot_reports_every_register() {
        let mut sup = Supervisor::new(2);
        sup.tick(&active(&[(1, Fault::RunHook(2))]), &[0]);
        sup.note_hook_finished();
        sup.note_sometimes(3);
        let snap = sup.snapshot();
        assert_eq!(snap.ticks, 1);
        assert_eq!(snap.alive, 0b11);
        assert_eq!(snap.hooks_started, 1);
        assert_eq!(snap.hooks_finished, 1);
        assert_eq!(snap.sometimes, 1 << 3);
        assert_eq!(snap.unexpected_deaths, 1);
        assert_eq!(snap.restarts, 1);
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
                    hold_nanos: 2_000_000
                }
            )
            .describe(),
            "park node 0 at 0x4b0e86 hit 28 hold 2000000"
        );
        assert_eq!(Action::Unpark(0).describe(), "unpark node 0");
    }
}
