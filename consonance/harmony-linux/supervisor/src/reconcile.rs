// SPDX-License-Identifier: AGPL-3.0-or-later

use process_proto::events::ParkTarget;
use process_proto::{ProcessAction, ProcessWindow, WireError, decode_process_windows};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventPark {
    pub edges: u32,
    pub hold_nanos: u64,
    pub target: Option<ParkTarget>,
    pub start: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EventKillWindow {
    pub node: u16,
    pub rarity: u8,
    pub start: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NodeActions {
    pub kill: bool,
    pub pause: bool,
    pub restart: bool,
    pub event_park: Option<EventPark>,
}

impl NodeActions {
    #[must_use]
    pub fn any(self) -> bool {
        self.kill || self.pause || self.restart
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HookWindow {
    pub id: u32,
    pub start: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActiveWindows {
    nodes: Vec<(u16, NodeActions)>,
    hooks: Vec<HookWindow>,
    event_kills: Vec<EventKillWindow>,
}

impl ActiveWindows {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, node: u16, action: &ProcessAction, start: u64) {
        match action {
            ProcessAction::RunHook(id) => {
                let window = HookWindow { id: *id, start };
                if let Err(at) = self.hooks.binary_search(&window) {
                    self.hooks.insert(at, window);
                }
                return;
            }
            ProcessAction::EventKill { rarity } => {
                let window = EventKillWindow {
                    node,
                    rarity: *rarity,
                    start,
                };
                if let Err(at) = self.event_kills.binary_search(&window) {
                    self.event_kills.insert(at, window);
                }
                return;
            }
            ProcessAction::Kill
            | ProcessAction::Pause(_)
            | ProcessAction::Restart
            | ProcessAction::EventPark { .. } => {}
        }
        let index = match self.nodes.binary_search_by_key(&node, |entry| entry.0) {
            Ok(index) => index,
            Err(at) => {
                self.nodes.insert(at, (node, NodeActions::default()));
                at
            }
        };
        let flags = &mut self.nodes[index].1;
        match action {
            ProcessAction::Kill => flags.kill = true,
            ProcessAction::Pause(_) => flags.pause = true,
            ProcessAction::Restart => flags.restart = true,
            ProcessAction::EventPark {
                edges,
                hold_nanos,
                target,
            } => {
                flags.event_park = Some(EventPark {
                    edges: *edges,
                    hold_nanos: *hold_nanos,
                    target: *target,
                    start,
                });
            }
            ProcessAction::EventKill { .. } | ProcessAction::RunHook(_) => {}
        }
    }

    pub fn from_answer(body: &[u8]) -> Result<Self, WireError> {
        let (_moment, windows) = decode_process_windows(body)?;
        Ok(Self::from_windows(windows.iter()))
    }

    pub fn from_windows<'a>(windows: impl Iterator<Item = &'a ProcessWindow>) -> Self {
        let mut active = Self::new();
        for window in windows {
            active.insert(window.node, &window.action, window.start);
        }
        active
    }

    #[must_use]
    pub fn node(&self, node: u16) -> NodeActions {
        self.nodes
            .binary_search_by_key(&node, |entry| entry.0)
            .map(|index| self.nodes[index].1)
            .unwrap_or_default()
    }

    #[must_use]
    pub fn hooks(&self) -> &[HookWindow] {
        &self.hooks
    }

    #[must_use]
    pub fn event_kill(&self, node: u16, fired: &[EventKillWindow]) -> Option<EventKillWindow> {
        self.event_kills
            .iter()
            .find(|window| window.node == node && fired.binary_search(window).is_err())
            .copied()
    }

    #[must_use]
    pub fn event_kill_windows(&self) -> &[EventKillWindow] {
        &self.event_kills
    }

    #[must_use]
    pub fn pending_process_faults(&self, fired: &[EventKillWindow]) -> u64 {
        let mut pending = 0_u64;
        for (_, actions) in &self.nodes {
            pending = pending.saturating_add(u64::from(actions.kill));
            pending = pending.saturating_add(u64::from(actions.pause));
            pending = pending.saturating_add(u64::from(actions.restart));
            pending = pending.saturating_add(u64::from(actions.event_park.is_some()));
        }
        let mut last_pending_node = None;
        for window in &self.event_kills {
            if fired.binary_search(window).is_err() && last_pending_node != Some(window.node) {
                pending = pending.saturating_add(1);
                last_pending_node = Some(window.node);
            }
        }
        pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use process_proto::{PROCESS_CLASS, Window, encode_standing, encode_target};

    #[test]
    fn process_windows_decode_into_per_node_flags() {
        let windows = [
            ProcessWindow {
                node: 1,
                action: ProcessAction::Pause(5),
                start: 0,
                end: 1,
            },
            ProcessWindow {
                node: 0,
                action: ProcessAction::Kill,
                start: 0,
                end: 1,
            },
            ProcessWindow {
                node: 1,
                action: ProcessAction::Restart,
                start: 0,
                end: 1,
            },
            ProcessWindow {
                node: 0,
                action: ProcessAction::RunHook(9),
                start: 0,
                end: 1,
            },
            ProcessWindow {
                node: 0,
                action: ProcessAction::RunHook(2),
                start: 0,
                end: 1,
            },
        ];
        let active = ActiveWindows::from_windows(windows.iter());
        assert_eq!(
            active.node(0),
            NodeActions {
                kill: true,
                ..NodeActions::default()
            }
        );
        assert_eq!(
            active.node(1),
            NodeActions {
                pause: true,
                restart: true,
                kill: false,
                event_park: None,
            }
        );
        assert_eq!(active.node(2), NodeActions::default());
        assert!(!active.node(2).any());
        assert_eq!(
            active
                .hooks()
                .iter()
                .map(|window| window.id)
                .collect::<Vec<_>>(),
            [2, 9]
        );
    }

    #[test]
    fn other_classes_and_undecodable_targets_are_skipped() {
        let good = encode_target(3, &ProcessAction::Kill);
        let body = encode_standing(
            0,
            &[
                Window {
                    class: PROCESS_CLASS + 1,
                    target: good.clone(),
                    start: 0,
                    end: 1,
                },
                Window {
                    class: PROCESS_CLASS,
                    target: good[..1].to_vec(),
                    start: 0,
                    end: 1,
                },
                Window {
                    class: PROCESS_CLASS,
                    target: Vec::new(),
                    start: 0,
                    end: 1,
                },
                Window {
                    class: PROCESS_CLASS,
                    target: good,
                    start: 0,
                    end: 1,
                },
            ],
        )
        .unwrap();
        let active = ActiveWindows::from_answer(&body).unwrap();
        assert!(active.node(3).kill);
        assert!(!active.node(0).any());
        assert!(active.hooks().is_empty());
    }

    #[test]
    fn a_repeated_window_is_idempotent() {
        let mut active = ActiveWindows::new();
        for _ in 0..3 {
            active.insert(7, &ProcessAction::Kill, 0);
            active.insert(7, &ProcessAction::RunHook(1), 0);
        }
        assert!(active.node(7).kill);
        assert_eq!(
            active
                .hooks()
                .iter()
                .map(|window| window.id)
                .collect::<Vec<_>>(),
            [1]
        );
    }

    #[test]
    fn touching_windows_for_one_hook_are_distinct() {
        let mut active = ActiveWindows::new();
        active.insert(0, &ProcessAction::RunHook(2), 500);
        active.insert(0, &ProcessAction::RunHook(2), 0);
        active.insert(0, &ProcessAction::RunHook(1), 500);
        assert_eq!(
            active.hooks(),
            [
                HookWindow { id: 1, start: 500 },
                HookWindow { id: 2, start: 0 },
                HookWindow { id: 2, start: 500 },
            ]
        );
    }

    #[test]
    fn overlapping_event_kill_windows_have_a_canonical_first_window() {
        let mut active = ActiveWindows::new();
        active.insert(0, &ProcessAction::EventKill { rarity: 7 }, 20);
        active.insert(0, &ProcessAction::EventKill { rarity: 3 }, 10);
        active.insert(0, &ProcessAction::EventKill { rarity: 7 }, 20);
        assert_eq!(
            active.event_kill(0, &[]),
            Some(EventKillWindow {
                node: 0,
                rarity: 3,
                start: 10,
            })
        );
        assert_eq!(active.event_kill_windows().len(), 2);
    }

    #[test]
    fn pending_process_faults_advance_past_a_fired_event_window() {
        let mut active = ActiveWindows::new();
        active.insert(0, &ProcessAction::EventKill { rarity: 7 }, 20);
        active.insert(0, &ProcessAction::EventKill { rarity: 3 }, 10);
        active.insert(0, &ProcessAction::Pause(4), 10);
        let canonical = EventKillWindow {
            node: 0,
            rarity: 3,
            start: 10,
        };
        assert_eq!(active.pending_process_faults(&[]), 2);
        assert_eq!(active.pending_process_faults(&[canonical]), 2);
        let later = EventKillWindow {
            node: 0,
            rarity: 7,
            start: 20,
        };
        assert_eq!(active.pending_process_faults(&[canonical, later]), 1);
    }
}
