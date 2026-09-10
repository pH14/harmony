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
//! | `ProcEventKill` | arm the instrumented runtime at an event ordinal | disarm the arm |
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
    /// Arm a running instrumented node to synchronously kill its process group
    /// at an ordinal in the deterministic event stream.
    ArmEventKill(u16, u64),
    /// Remove an event-kill arm from a running instrumented node.
    DisarmEventKill(u16),
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
            Action::ArmEventKill(node, ordinal) => {
                format!("arm event kill node {node} ordinal {ordinal}")
            }
            Action::DisarmEventKill(node) => format!("disarm event kill node {node}"),
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
    /// Node exits not expected from Kill or Restart.
    pub unexpected_deaths: u64,
    /// Node deaths observed while the node's EventKill arm was active.
    pub event_kills_fired: u64,
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
    /// The EventKill arm currently installed in this process, if any. The
    /// standing-fault answer can close before the process exit is reaped, so
    /// this process-owned state is what identifies the death's arm.
    event_kill_armed: Option<u64>,
}

/// The agent's model of its nodes and the last answer it applied.
#[derive(Clone, Debug)]
pub struct Supervisor {
    nodes: Vec<NodeState>,
    previous: ActiveFaults,
    counters: Counters,
    /// The next incarnation id for a managed process built with the
    /// instrumented event runtime. This lives in the guest-resident
    /// supervisor state so VM snapshots and replay preserve the allocator.
    /// Zero is the exhausted marker; every assigned id is positive.
    next_instrumented_process_id: u32,
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
                    event_kill_armed: None,
                };
                node_count
            ],
            previous: ActiveFaults::new(),
            counters: Counters::default(),
            next_instrumented_process_id: 1,
        }
    }

    /// Allocate the next process incarnation id for an instrumented managed
    /// node. The id is global to the supervisor, so starts across all nodes
    /// cannot reuse one another's ids during an agent execution.
    pub fn allocate_instrumented_process_id(&mut self) -> Result<u32, String> {
        let id = self.next_instrumented_process_id;
        if id == 0 {
            return Err("instrumented process incarnation id space exhausted".to_owned());
        }
        self.next_instrumented_process_id = if id == i32::MAX as u32 { 0 } else { id + 1 };
        Ok(id)
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
            let event_kill_armed = state.event_kill_armed.take().is_some();
            if !state.expected_down {
                self.counters.unexpected_deaths += 1;
                if event_kill_armed {
                    self.counters.event_kills_fired += 1;
                }
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
                    state.event_kill_armed = None;
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
            if now.event_kill != was.event_kill {
                match now.event_kill {
                    Some(ordinal) if state.alive => {
                        actions.push(Action::ArmEventKill(node, ordinal));
                        state.event_kill_armed = Some(ordinal);
                    }
                    None if was.event_kill.is_some() && state.alive => {
                        actions.push(Action::DisarmEventKill(node));
                        state.event_kill_armed = None;
                    }
                    _ => {}
                }
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
                state.event_kill_armed = None;
                self.counters.restarts += 1;
                // Event-kill is a one-shot arm owned by the process.  If the
                // process died while its standing window remained open, the
                // replacement needs the same arm after it starts.
                if let Some(ordinal) = now.event_kill {
                    actions.push(Action::ArmEventKill(node, ordinal));
                    state.event_kill_armed = Some(ordinal);
                }
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
            self.nodes[index].event_kill_armed = None;
            self.counters.restarts += 1;
            // A standing event-kill window survives the crash, but its arm
            // does not: it lives in the old process.  Re-arm the replacement
            // in action order, immediately after its Start action.
            if let Some(ordinal) = active.node(node).event_kill {
                actions.push(Action::ArmEventKill(node, ordinal));
                self.nodes[index].event_kill_armed = Some(ordinal);
            }
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
            event_kills_fired: self.counters.event_kills_fired,
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
            set.insert(*node, fault, 0);
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
    fn an_event_kill_window_arms_and_disarms_without_signalling_at_the_boundary() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, Fault::ProcEventKill { ordinal: 19 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 19)]);
        assert_eq!(sup.tick(&event, &[]), []);
        assert_eq!(
            sup.tick(&ActiveFaults::new(), &[]),
            [Action::DisarmEventKill(0)]
        );
        assert_eq!(sup.alive_bitmap(), 1);
    }

    #[test]
    fn an_event_kill_window_rearms_a_replacement_process() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, Fault::ProcEventKill { ordinal: 19 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 19)]);

        // The event arm kills the process.  The standing window is unchanged,
        // so the replacement must be armed explicitly after it starts.
        assert_eq!(
            sup.tick(&event, &[0]),
            [Action::Start(0), Action::ArmEventKill(0, 19)]
        );

        // The re-armed process can die again and is treated identically while
        // the window remains open.
        assert_eq!(
            sup.tick(&event, &[0]),
            [Action::Start(0), Action::ArmEventKill(0, 19)]
        );

        // Once the window closes, the live replacement is disarmed normally.
        assert_eq!(
            sup.tick(&ActiveFaults::new(), &[]),
            [Action::DisarmEventKill(0)]
        );
    }

    #[test]
    fn an_event_kill_death_has_a_dedicated_fired_counter() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, Fault::ProcEventKill { ordinal: 19 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 19)]);
        assert_eq!(
            sup.tick(&event, &[0]),
            [Action::Start(0), Action::ArmEventKill(0, 19)]
        );
        assert_eq!(sup.counters().event_kills_fired, 1);
        // The generic death register retains its existing meaning and still
        // records the process exit as an unexpected death.
        assert_eq!(sup.counters().unexpected_deaths, 1);
    }

    #[test]
    fn an_event_kill_death_is_counted_after_its_window_closes() {
        let mut sup = Supervisor::new(1);
        let event = active(&[(0, Fault::ProcEventKill { ordinal: 19 })]);
        assert_eq!(sup.tick(&event, &[]), [Action::ArmEventKill(0, 19)]);
        // The process exits before the next standing poll observes that the
        // EventKill window has closed. The installed arm is still evidence.
        assert_eq!(sup.tick(&ActiveFaults::new(), &[0]), [Action::Start(0)]);
        assert_eq!(sup.counters().event_kills_fired, 1);
    }

    #[test]
    fn a_kill_expected_death_does_not_increment_the_event_counter() {
        let mut sup = Supervisor::new(1);
        let both = active(&[
            (0, Fault::ProcKill),
            (0, Fault::ProcEventKill { ordinal: 19 }),
        ]);
        assert_eq!(sup.tick(&both, &[]), [Action::Kill(0)]);
        assert_eq!(sup.tick(&both, &[0]), []);
        assert_eq!(sup.counters().event_kills_fired, 0);
        assert_eq!(sup.counters().unexpected_deaths, 0);
    }

    #[test]
    fn an_event_kill_rearms_after_restart_window_closes() {
        let mut sup = Supervisor::new(1);
        let both = active(&[
            (0, Fault::ProcEventKill { ordinal: 23 }),
            (0, Fault::ProcRestart),
        ]);
        // Restart takes the running process down, so there is no control
        // channel to arm yet.
        assert_eq!(sup.tick(&both, &[]), [Action::Kill(0)]);
        assert_eq!(sup.tick(&both, &[0]), []);

        // The restart closes while event-kill remains standing.  Start first,
        // then arm the new process.
        let event = active(&[(0, Fault::ProcEventKill { ordinal: 23 })]);
        assert_eq!(
            sup.tick(&event, &[]),
            [Action::Start(0), Action::ArmEventKill(0, 23)]
        );
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
    fn a_touching_window_for_the_same_hook_launches_it_again() {
        let mut sup = Supervisor::new(1);
        let mut first = ActiveFaults::new();
        first.insert(0, &Fault::RunHook(2), 0);
        assert_eq!(sup.tick(&first, &[]), [Action::RunHook(2)]);
        // No poll saw the gap between the windows: the first closed and the
        // next opened between two ticks.
        let mut second = ActiveFaults::new();
        second.insert(0, &Fault::RunHook(2), 500);
        assert_eq!(sup.tick(&second, &[]), [Action::RunHook(2)]);
        assert_eq!(sup.tick(&second, &[]), []);
        assert_eq!(sup.counters().hooks_started, 2);
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
        assert_eq!(snap.event_kills_fired, 0);
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

    #[test]
    fn instrumented_process_ids_are_global_across_nodes() {
        let mut sup = Supervisor::new(2);
        assert_eq!(sup.allocate_instrumented_process_id(), Ok(1));
        assert_eq!(sup.allocate_instrumented_process_id(), Ok(2));
        assert_eq!(sup.allocate_instrumented_process_id(), Ok(3));
    }

    #[test]
    fn instrumented_process_id_state_replays_from_a_snapshot() {
        let mut sup = Supervisor::new(1);
        assert_eq!(sup.allocate_instrumented_process_id(), Ok(1));
        let mut replay = sup.clone();
        assert_eq!(sup.allocate_instrumented_process_id(), Ok(2));
        assert_eq!(replay.allocate_instrumented_process_id(), Ok(2));
    }

    #[test]
    fn instrumented_process_id_overflow_fails_loudly() {
        let mut sup = Supervisor::new(1);
        sup.next_instrumented_process_id = i32::MAX as u32;
        assert_eq!(sup.allocate_instrumented_process_id(), Ok(i32::MAX as u32));
        assert_eq!(
            sup.allocate_instrumented_process_id(),
            Err("instrumented process incarnation id space exhausted".to_owned())
        );
    }
}
