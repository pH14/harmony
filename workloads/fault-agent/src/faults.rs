// SPDX-License-Identifier: AGPL-3.0-or-later

use fault_policy::{DecisionClass, EnvError, Fault, decode_process_target, parse_standing};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Park {
    pub addr: u64,
    pub hits: u32,
    pub hold_nanos: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventPark {
    pub rarity: u8,
    pub hold_nanos: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EventKillWindow {
    pub node: u16,
    pub rarity: u8,
    pub start: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NodeFaults {
    pub kill: bool,
    pub pause: bool,
    pub restart: bool,
    pub park: Option<Park>,
    pub event_kill: Option<u8>,
    pub event_park: Option<EventPark>,
}

impl NodeFaults {
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
pub struct ActiveFaults {
    nodes: Vec<(u16, NodeFaults)>,
    hooks: Vec<HookWindow>,
    event_kills: Vec<EventKillWindow>,
}

impl ActiveFaults {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, node: u16, fault: &Fault, start: u64) {
        match fault {
            Fault::RunHook(id) => {
                let window = HookWindow { id: *id, start };
                if let Err(at) = self.hooks.binary_search(&window) {
                    self.hooks.insert(at, window);
                }
                return;
            }
            Fault::ProcEventKill { rarity } => {
                let window = EventKillWindow {
                    node,
                    rarity: *rarity,
                    start,
                };
                if let Err(at) = self.event_kills.binary_search(&window) {
                    self.event_kills.insert(at, window);
                }
                self.refresh_event_kill(node);
                return;
            }
            Fault::ProcKill
            | Fault::ProcPause(_)
            | Fault::ProcRestart
            | Fault::ProcPark { .. }
            | Fault::ProcEventPark { .. } => {}
            _ => return,
        }
        let index = match self.nodes.binary_search_by_key(&node, |entry| entry.0) {
            Ok(index) => index,
            Err(at) => {
                self.nodes.insert(at, (node, NodeFaults::default()));
                at
            }
        };
        let flags = &mut self.nodes[index].1;
        match fault {
            Fault::ProcKill => flags.kill = true,
            Fault::ProcPause(_) => flags.pause = true,
            Fault::ProcRestart => flags.restart = true,
            Fault::ProcPark { addr, hits, hold } => {
                flags.park = Some(Park {
                    addr: *addr,
                    hits: *hits,
                    hold_nanos: hold.0,
                });
            }
            Fault::ProcEventKill { rarity } => flags.event_kill = Some(*rarity),
            Fault::ProcEventPark { rarity, hold } => {
                flags.event_park = Some(EventPark {
                    rarity: *rarity,
                    hold_nanos: hold.0,
                });
            }
            _ => {}
        }
    }

    fn refresh_event_kill(&mut self, node: u16) {
        let rarity = self
            .event_kills
            .iter()
            .find(|window| window.node == node)
            .map(|window| window.rarity);
        let index = match self.nodes.binary_search_by_key(&node, |entry| entry.0) {
            Ok(index) => index,
            Err(at) => {
                self.nodes.insert(at, (node, NodeFaults::default()));
                at
            }
        };
        self.nodes[index].1.event_kill = rarity;
    }

    pub fn from_answer(body: &[u8]) -> Result<Self, EnvError> {
        let (_moment, entries) = parse_standing(body)?;
        Ok(Self::from_entries(
            entries.map(|entry| (entry.class, entry.target, entry.start)),
        ))
    }

    pub fn from_entries<'a>(entries: impl Iterator<Item = (u16, &'a [u8], u64)>) -> Self {
        let mut active = Self::new();
        for (class, target, start) in entries {
            if class != DecisionClass::Process.as_u16() {
                continue;
            }
            if let Some((node, fault)) = decode_process_target(target) {
                active.insert(node, &fault, start);
            }
        }
        active
    }

    #[must_use]
    pub fn node(&self, node: u16) -> NodeFaults {
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
    pub fn event_kill(&self, node: u16) -> Option<EventKillWindow> {
        self.event_kills
            .iter()
            .find(|window| window.node == node)
            .copied()
    }

    #[must_use]
    pub fn event_kill_windows(&self) -> &[EventKillWindow] {
        &self.event_kills
    }

    #[must_use]
    pub fn pending_process_faults(&self, fired: &[EventKillWindow]) -> u64 {
        let mut pending = 0_u64;
        for (node, faults) in &self.nodes {
            pending = pending.saturating_add(u64::from(faults.kill));
            pending = pending.saturating_add(u64::from(faults.pause));
            pending = pending.saturating_add(u64::from(faults.restart));
            pending = pending.saturating_add(u64::from(faults.park.is_some()));
            pending = pending.saturating_add(u64::from(faults.event_park.is_some()));
            if let Some(window) = self.event_kills.iter().find(|window| window.node == *node)
                && fired.binary_search(window).is_err()
            {
                pending = pending.saturating_add(1);
            }
        }
        pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fault_policy::{Span, process_target};

    fn target(node: u16, fault: &Fault) -> Vec<u8> {
        process_target(node, fault)
    }

    #[test]
    fn process_entries_decode_into_per_node_flags() {
        let process = DecisionClass::Process.as_u16();
        let entries = [
            (process, target(1, &Fault::ProcPause(Span(5)))),
            (process, target(0, &Fault::ProcKill)),
            (process, target(1, &Fault::ProcRestart)),
            (process, target(0, &Fault::RunHook(9))),
            (process, target(0, &Fault::RunHook(2))),
            (process, target(0, &Fault::ProcEventKill { rarity: 4 })),
            (
                process,
                target(
                    1,
                    &Fault::ProcEventPark {
                        rarity: 3,
                        hold: Span(8),
                    },
                ),
            ),
        ];
        let active = ActiveFaults::from_entries(entries.iter().map(|(c, t)| (*c, t.as_slice(), 0)));
        assert_eq!(
            active.node(0),
            NodeFaults {
                kill: true,
                event_kill: Some(4),
                ..NodeFaults::default()
            }
        );
        assert_eq!(
            active.node(1),
            NodeFaults {
                pause: true,
                restart: true,
                kill: false,
                park: None,
                event_kill: None,
                event_park: Some(EventPark {
                    rarity: 3,
                    hold_nanos: 8,
                }),
            }
        );
        assert_eq!(active.node(2), NodeFaults::default());
        assert!(!active.node(2).any());
        assert_eq!(hook_ids(&active), [2, 9]);
    }

    #[test]
    fn other_classes_and_undecodable_targets_are_skipped() {
        let process = DecisionClass::Process.as_u16();
        let good = target(3, &Fault::ProcKill);
        let entries: Vec<(u16, &[u8], u64)> = vec![
            (DecisionClass::BlockIo.as_u16(), good.as_slice(), 0),
            (process, &good[..1], 0),
            (process, &[], 0),
            (process, b"\x00\x00\x0b\xff\xff", 0),
            (process, good.as_slice(), 0),
        ];
        let active = ActiveFaults::from_entries(entries.into_iter());
        assert!(active.node(3).kill);
        assert!(!active.node(0).any());
        assert!(active.hooks().is_empty());
    }

    #[test]
    fn a_park_decodes_into_its_parameters() {
        let process = DecisionClass::Process.as_u16();
        let park = Fault::ProcPark {
            addr: 0x4b0e86,
            hits: 28,
            hold: Span(2_000_000),
        };
        let entries = [(process, target(1, &park))];
        let active = ActiveFaults::from_entries(entries.iter().map(|(c, t)| (*c, t.as_slice(), 0)));
        assert_eq!(
            active.node(1).park,
            Some(Park {
                addr: 0x4b0e86,
                hits: 28,
                hold_nanos: 2_000_000,
            })
        );
        assert!(!active.node(1).any());
    }

    #[test]
    fn a_repeated_fault_is_idempotent() {
        let mut active = ActiveFaults::new();
        for _ in 0..3 {
            active.insert(7, &Fault::ProcKill, 0);
            active.insert(7, &Fault::RunHook(1), 0);
        }
        assert!(active.node(7).kill);
        assert_eq!(hook_ids(&active), [1]);
    }

    #[test]
    fn touching_windows_for_one_hook_are_distinct() {
        let mut active = ActiveFaults::new();
        active.insert(0, &Fault::RunHook(2), 500);
        active.insert(0, &Fault::RunHook(2), 0);
        active.insert(0, &Fault::RunHook(1), 500);
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
        let mut active = ActiveFaults::new();
        active.insert(0, &Fault::ProcEventKill { rarity: 7 }, 20);
        active.insert(0, &Fault::ProcEventKill { rarity: 3 }, 10);
        active.insert(0, &Fault::ProcEventKill { rarity: 7 }, 20);
        assert_eq!(
            active.event_kill(0),
            Some(EventKillWindow {
                node: 0,
                rarity: 3,
                start: 10,
            })
        );
        assert_eq!(active.event_kill_windows().len(), 2);
        assert_eq!(active.node(0).event_kill, Some(3));
    }

    #[test]
    fn pending_process_faults_ignore_a_fired_canonical_event_window() {
        let mut active = ActiveFaults::new();
        active.insert(0, &Fault::ProcEventKill { rarity: 7 }, 20);
        active.insert(0, &Fault::ProcEventKill { rarity: 3 }, 10);
        active.insert(0, &Fault::ProcPause(Span(4)), 10);
        let canonical = EventKillWindow {
            node: 0,
            rarity: 3,
            start: 10,
        };
        assert_eq!(active.pending_process_faults(&[]), 2);
        assert_eq!(active.pending_process_faults(&[canonical]), 1);
    }

    #[test]
    fn faults_of_other_classes_are_ignored_when_inserted_directly() {
        let mut active = ActiveFaults::new();
        active.insert(0, &Fault::BuggifyFire, 0);
        assert_eq!(active, ActiveFaults::new());
    }

    fn hook_ids(active: &ActiveFaults) -> Vec<u32> {
        active.hooks().iter().map(|window| window.id).collect()
    }
}
