// SPDX-License-Identifier: AGPL-3.0-or-later

use process_proto::{ProcessAction, ROLE_NODE};

use crate::reconcile::Standing;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RolePause {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Binding {
    pause: RolePause,
    node: Option<u16>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RoleWindows {
    bindings: Vec<Binding>,
}

impl RoleWindows {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind(&mut self, standing: &mut Standing) -> Option<RolePause> {
        let moment = standing.moment;
        let mut used = vec![false; self.bindings.len()];
        let mut ask = None;
        let bindings = &self.bindings;
        standing.windows.retain_mut(|window| {
            if window.node != ROLE_NODE {
                return true;
            }
            let pause = matches!(window.action, ProcessAction::Pause(_));
            let found = bindings.iter().position(|binding| match window.action {
                ProcessAction::Pause(_) => {
                    binding.pause.start == window.start && binding.pause.end == window.end
                }
                ProcessAction::EventPark {
                    at_resume: true, ..
                } => binding.pause.end == window.start,
                _ => false,
            });
            let Some(index) = found else {
                if pause && window.start <= moment && ask.is_none() {
                    ask = Some(RolePause {
                        start: window.start,
                        end: window.end,
                    });
                }
                return false;
            };
            used[index] = true;
            match bindings[index].node {
                Some(node) => {
                    window.node = node;
                    true
                }
                None => false,
            }
        });
        let mut index = 0;
        self.bindings.retain(|binding| {
            let keep = used[index] || binding.pause.end >= moment;
            index += 1;
            keep
        });
        ask
    }

    pub fn resolve(&mut self, pause: RolePause, node: Option<u16>, moment: u64) -> Option<u16> {
        let node = node.filter(|_| moment < pause.end);
        self.bindings.push(Binding { pause, node });
        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use process_proto::ProcessWindow;

    fn window(node: u16, action: ProcessAction, start: u64, end: u64) -> ProcessWindow {
        ProcessWindow {
            node,
            action,
            start,
            end,
        }
    }

    fn park() -> ProcessAction {
        ProcessAction::EventPark {
            edges: 1,
            hold_nanos: 5,
            target: None,
            at_resume: true,
        }
    }

    fn standing(moment: u64, windows: Vec<ProcessWindow>) -> Standing {
        Standing { moment, windows }
    }

    #[test]
    fn a_role_pause_waits_unresolved_until_its_node_is_known_and_its_park_follows_it() {
        let mut roles = RoleWindows::new();
        let windows = vec![
            window(0, ProcessAction::Kill, 0, 100),
            window(ROLE_NODE, ProcessAction::Pause(50), 10, 60),
            window(ROLE_NODE, park(), 60, 110),
        ];
        let mut early = standing(5, windows.clone());
        assert_eq!(roles.bind(&mut early), None);
        assert_eq!(early.windows, windows[..1]);

        let mut started = standing(12, windows.clone());
        let pause = RolePause { start: 10, end: 60 };
        assert_eq!(roles.bind(&mut started), Some(pause));
        assert_eq!(started.windows, windows[..1]);
        assert_eq!(roles.resolve(pause, Some(2), 14), Some(2));

        let mut paused = standing(20, windows.clone());
        assert_eq!(roles.bind(&mut paused), None);
        assert_eq!(
            paused.windows,
            [
                windows[0],
                window(2, ProcessAction::Pause(50), 10, 60),
                window(2, park(), 60, 110),
            ]
        );

        let mut parked = standing(80, windows[2..].to_vec());
        assert_eq!(roles.bind(&mut parked), None);
        assert_eq!(parked.windows, [window(2, park(), 60, 110)]);
    }

    #[test]
    fn an_unresolved_role_pause_and_its_park_stay_out_of_every_step() {
        let mut roles = RoleWindows::new();
        let windows = vec![
            window(ROLE_NODE, ProcessAction::Pause(50), 10, 60),
            window(ROLE_NODE, park(), 60, 110),
        ];
        let pause = RolePause { start: 10, end: 60 };
        assert_eq!(roles.bind(&mut standing(10, windows.clone())), Some(pause));
        assert_eq!(roles.resolve(pause, None, 11), None);
        let mut later = standing(70, windows);
        assert_eq!(roles.bind(&mut later), None);
        assert!(later.windows.is_empty());
    }

    #[test]
    fn a_role_known_only_after_its_pause_ends_is_unresolved() {
        let mut roles = RoleWindows::new();
        let pause = RolePause { start: 10, end: 60 };
        assert_eq!(roles.resolve(pause, Some(1), 60), None);
        let mut parked = standing(61, vec![window(ROLE_NODE, park(), 60, 110)]);
        assert_eq!(roles.bind(&mut parked), None);
        assert!(parked.windows.is_empty());
    }

    #[test]
    fn a_binding_is_dropped_once_no_window_of_its_action_stands() {
        let mut roles = RoleWindows::new();
        let pause = RolePause { start: 10, end: 60 };
        roles.resolve(pause, Some(0), 20);
        roles.bind(&mut standing(200, Vec::new()));
        let mut again = standing(
            200,
            vec![window(ROLE_NODE, ProcessAction::Pause(50), 10, 60)],
        );
        assert_eq!(roles.bind(&mut again), Some(pause));
    }
}
