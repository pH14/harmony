// SPDX-License-Identifier: AGPL-3.0-or-later

use process_proto::{ProcessAction, ProcessWindow, WireError, decode_process_windows};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Park {
    pub addr: u64,
    pub hits: u32,
    pub hold_nanos: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NodeActions {
    pub kill: bool,
    pub pause: bool,
    pub restart: bool,
    pub park: Option<Park>,
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
            ProcessAction::Kill
            | ProcessAction::Pause(_)
            | ProcessAction::Restart
            | ProcessAction::Park { .. } => {}
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
            ProcessAction::Park {
                addr,
                hits,
                hold_nanos,
            } => {
                flags.park = Some(Park {
                    addr: *addr,
                    hits: *hits,
                    hold_nanos: *hold_nanos,
                });
            }
            ProcessAction::RunHook(_) => {}
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
                park: None,
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
    fn a_park_decodes_into_its_parameters() {
        let windows = [ProcessWindow {
            node: 1,
            action: ProcessAction::Park {
                addr: 0x4b_0e86,
                hits: 28,
                hold_nanos: 2_000_000,
            },
            start: 0,
            end: 1,
        }];
        let active = ActiveWindows::from_windows(windows.iter());
        assert_eq!(
            active.node(1).park,
            Some(Park {
                addr: 0x4b_0e86,
                hits: 28,
                hold_nanos: 2_000_000,
            })
        );
        assert!(!active.node(1).any());
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
}
