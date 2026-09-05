// SPDX-License-Identifier: AGPL-3.0-or-later
//! The decode of one standing-poll answer into the set of faults in force.
//!
//! The host answers with the entries whose half-open V-time window contains the
//! current `Moment`, each carrying a `DecisionClass` discriminant and opaque
//! class-interpreted target bytes. This module keeps the `Process` entries,
//! decodes their targets with the encoding `environment` shares with the host,
//! and presents them in a canonical order so the reconciliation that follows is
//! a function of the answer alone.

use environment::{DecisionClass, Fault, decode_process_target};

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
}

impl NodeFaults {
    /// Whether any fault is in force for this node.
    #[must_use]
    pub fn any(self) -> bool {
        self.kill || self.pause || self.restart
    }
}

/// Every process fault in force at one `Moment`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActiveFaults {
    /// Sorted by node id; one entry per node named by at least one fault.
    nodes: Vec<(u16, NodeFaults)>,
    /// Sorted, deduplicated hook ids.
    hooks: Vec<u32>,
}

impl ActiveFaults {
    /// An empty set: no fault in force.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one decoded fault against a node.
    pub fn insert(&mut self, node: u16, fault: &Fault) {
        match fault {
            Fault::RunHook(id) => {
                if let Err(at) = self.hooks.binary_search(id) {
                    self.hooks.insert(at, *id);
                }
                return;
            }
            Fault::ProcKill | Fault::ProcPause(_) | Fault::ProcRestart => {}
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
            _ => {}
        }
    }

    /// Build the set from a standing-poll answer's `(class, target)` pairs.
    ///
    /// Entries of another class and targets that do not decode are skipped: the
    /// answer is untrusted input, and an entry the guest cannot interpret is
    /// the host's to apply, not a reason to stop supervising.
    pub fn from_entries<'a>(entries: impl Iterator<Item = (u16, &'a [u8])>) -> Self {
        let mut active = Self::new();
        for (class, target) in entries {
            if class != DecisionClass::Process.as_u16() {
                continue;
            }
            if let Some((node, fault)) = decode_process_target(target) {
                active.insert(node, &fault);
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

    /// The hook ids whose windows are open, ascending.
    #[must_use]
    pub fn hooks(&self) -> &[u32] {
        &self.hooks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment::{Span, process_target};

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
        let active = ActiveFaults::from_entries(entries.iter().map(|(c, t)| (*c, t.as_slice())));
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
                kill: false
            }
        );
        assert_eq!(active.node(2), NodeFaults::default());
        assert!(!active.node(2).any());
        // Hooks are ascending regardless of answer order, and hook faults do
        // not mark their node as faulted.
        assert_eq!(active.hooks(), [2, 9]);
    }

    #[test]
    fn other_classes_and_undecodable_targets_are_skipped() {
        let process = DecisionClass::Process.as_u16();
        let good = target(3, &Fault::ProcKill);
        let entries: Vec<(u16, &[u8])> = vec![
            // A class the guest does not apply, carrying bytes that would
            // decode as a kill if the class were ignored.
            (DecisionClass::BlockIo.as_u16(), good.as_slice()),
            // Truncated, empty, and trailing-byte targets.
            (process, &good[..1]),
            (process, &[]),
            (process, b"\x00\x00\x0b\xff\xff"),
            (process, good.as_slice()),
        ];
        let active = ActiveFaults::from_entries(entries.into_iter());
        assert!(active.node(3).kill);
        assert!(!active.node(0).any());
        assert!(active.hooks().is_empty());
    }

    #[test]
    fn a_repeated_fault_is_idempotent() {
        let mut active = ActiveFaults::new();
        for _ in 0..3 {
            active.insert(7, &Fault::ProcKill);
            active.insert(7, &Fault::RunHook(1));
        }
        assert!(active.node(7).kill);
        assert_eq!(active.hooks(), [1]);
    }

    #[test]
    fn faults_of_other_classes_are_ignored_when_inserted_directly() {
        let mut active = ActiveFaults::new();
        active.insert(0, &Fault::BuggifyFire);
        assert_eq!(active, ActiveFaults::new());
    }
}
