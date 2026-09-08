// SPDX-License-Identifier: AGPL-3.0-or-later
//! The decode of one standing-poll answer into the set of faults in force.
//!
//! The host answers with the entries whose half-open V-time window contains the
//! current `Moment`, each carrying a `DecisionClass` discriminant and opaque
//! class-interpreted target bytes. This module keeps the `Process` entries,
//! decodes their targets with the encoding `environment` shares with the host,
//! and presents them in a canonical order so the reconciliation that follows is
//! a function of the answer alone.

use fault_policy::{DecisionClass, EnvError, Fault, decode_process_target, parse_standing};

/// The parameters of a `Fault::ProcPark` window: the thread of the node that
/// reaches an instruction for the `hits`-th time is held there by the guest
/// kernel for `hold_nanos`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Park {
    /// User virtual address of the instruction.
    pub addr: u64,
    /// The hit that parks, counted from 1.
    pub hits: u32,
    /// Length of the hold in nanoseconds.
    pub hold_nanos: u64,
}

/// The process faults in force for one node.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NodeFaults {
    /// `Fault::ProcKill` — the node is killed and stays down.
    pub kill: bool,
    /// `Fault::ProcPause` — the node is stopped for the window's duration.
    pub pause: bool,
    /// `Fault::ProcRestart` — the node is killed and restarted when the window
    /// closes.
    pub restart: bool,
    /// `Fault::ProcPark` — a breakpoint is armed on the node for the window's
    /// duration, and the thread that takes its k-th hit is held there.
    pub park: Option<Park>,
}

impl NodeFaults {
    /// Whether any fault is in force for this node.
    #[must_use]
    pub fn any(self) -> bool {
        self.kill || self.pause || self.restart
    }
}

/// One open `Fault::RunHook` window. Two windows for one hook that touch
/// are told apart by their starts, so each of them launches the hook.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HookWindow {
    /// The hook id.
    pub id: u32,
    /// The window's inclusive V-time start.
    pub start: u64,
}

/// Every process fault in force at one `Moment`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActiveFaults {
    /// Sorted by node id; one entry per node named by at least one fault.
    nodes: Vec<(u16, NodeFaults)>,
    /// Sorted by hook id then start, deduplicated.
    hooks: Vec<HookWindow>,
}

impl ActiveFaults {
    /// An empty set: no fault in force.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one decoded fault against a node, from a window opening at
    /// `start`.
    pub fn insert(&mut self, node: u16, fault: &Fault, start: u64) {
        match fault {
            Fault::RunHook(id) => {
                let window = HookWindow { id: *id, start };
                if let Err(at) = self.hooks.binary_search(&window) {
                    self.hooks.insert(at, window);
                }
                return;
            }
            Fault::ProcKill | Fault::ProcPause(_) | Fault::ProcRestart | Fault::ProcPark { .. } => {
            }
            // Every other fault belongs to a class the guest does not apply;
            // the host enforces those itself.
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
            _ => {}
        }
    }

    /// Decode one standing-poll answer body.
    ///
    /// # Errors
    ///
    /// Returns the codec's error when the body is not a well-formed answer.
    pub fn from_answer(body: &[u8]) -> Result<Self, EnvError> {
        let (_moment, entries) = parse_standing(body)?;
        Ok(Self::from_entries(
            entries.map(|entry| (entry.class, entry.target, entry.start)),
        ))
    }

    /// Build the set from a standing-poll answer's `(class, target, start)`
    /// entries.
    ///
    /// Entries of another class and targets that do not decode are skipped: the
    /// answer is untrusted input, and an entry the guest cannot interpret is
    /// the host's to apply, not a reason to stop supervising.
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

    /// The faults in force for `node`; all clear when the answer does not name
    /// it.
    #[must_use]
    pub fn node(&self, node: u16) -> NodeFaults {
        self.nodes
            .binary_search_by_key(&node, |entry| entry.0)
            .map(|index| self.nodes[index].1)
            .unwrap_or_default()
    }

    /// The open hook windows, by hook id then start.
    #[must_use]
    pub fn hooks(&self) -> &[HookWindow] {
        &self.hooks
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
        ];
        let active = ActiveFaults::from_entries(entries.iter().map(|(c, t)| (*c, t.as_slice(), 0)));
        assert_eq!(
            active.node(0),
            NodeFaults {
                kill: true,
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
            }
        );
        assert_eq!(active.node(2), NodeFaults::default());
        assert!(!active.node(2).any());
        // Hooks are ascending regardless of answer order, and hook faults do
        // not mark their node as faulted.
        assert_eq!(hook_ids(&active), [2, 9]);
    }

    #[test]
    fn other_classes_and_undecodable_targets_are_skipped() {
        let process = DecisionClass::Process.as_u16();
        let good = target(3, &Fault::ProcKill);
        let entries: Vec<(u16, &[u8], u64)> = vec![
            // A class the guest does not apply, carrying bytes that would
            // decode as a kill if the class were ignored.
            (DecisionClass::BlockIo.as_u16(), good.as_slice(), 0),
            // Truncated, empty, and trailing-byte targets.
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
        // A park names the node without marking it faulted: the node keeps
        // running, so a death under it is still unexpected.
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
    fn faults_of_other_classes_are_ignored_when_inserted_directly() {
        let mut active = ActiveFaults::new();
        active.insert(0, &Fault::BuggifyFire, 0);
        assert_eq!(active, ActiveFaults::new());
    }

    fn hook_ids(active: &ActiveFaults) -> Vec<u32> {
        active.hooks().iter().map(|window| window.id).collect()
    }
}
