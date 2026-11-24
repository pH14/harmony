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
    start: u64,
    park_start: u64,
    from: u64,
    node: Option<u16>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Bound {
    pub ask: Option<RolePause>,
    pub expired: Vec<RolePause>,
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

    #[must_use]
    pub fn current(standing: &Standing, pause: RolePause) -> Option<RolePause> {
        standing
            .windows
            .iter()
            .find(|window| {
                window.node == ROLE_NODE
                    && matches!(window.action, ProcessAction::Pause(_))
                    && window.start == pause.start
            })
            .map(|window| RolePause {
                start: window.start,
                end: window.end,
            })
    }

    pub fn bind(&mut self, standing: &mut Standing, asking: Option<RolePause>) -> Bound {
        let moment = standing.moment;
        let mut bound = Bound::default();
        for window in &standing.windows {
            if window.node != ROLE_NODE || !matches!(window.action, ProcessAction::Pause(_)) {
                continue;
            }
            let pause = RolePause {
                start: window.start,
                end: window.end,
            };
            let known = self
                .bindings
                .iter()
                .any(|binding| binding.start == pause.start)
                || asking.is_some_and(|asking| asking.start == pause.start);
            if known {
                continue;
            }
            if pause.end <= moment {
                if !bound.expired.iter().any(|seen| seen.start == pause.start) {
                    bound.expired.push(pause);
                }
            } else if pause.start <= moment && asking.is_none() && bound.ask.is_none() {
                bound.ask = Some(pause);
            }
        }
        for pause in &bound.expired {
            self.bindings.push(Binding {
                start: pause.start,
                park_start: pause.end,
                from: moment,
                node: None,
            });
        }
        let mut used = vec![false; self.bindings.len()];
        let bindings = &self.bindings;
        standing.windows.retain_mut(|window| {
            if window.node != ROLE_NODE {
                return true;
            }
            match window.action {
                ProcessAction::Pause(length) => {
                    let Some(index) = bindings
                        .iter()
                        .position(|binding| binding.start == window.start)
                    else {
                        return false;
                    };
                    used[index] = true;
                    let binding = bindings[index];
                    let Some(node) = binding.node else {
                        return false;
                    };
                    window.node = node;
                    window.start = window
                        .start
                        .max(binding.from)
                        .max(window.end.saturating_sub(length));
                    window.start < window.end
                }
                ProcessAction::EventPark {
                    at_resume: true, ..
                } => {
                    let mut owners = bindings
                        .iter()
                        .enumerate()
                        .filter(|(_, binding)| binding.park_start == window.start);
                    let (Some((index, binding)), None) = (owners.next(), owners.next()) else {
                        return false;
                    };
                    used[index] = true;
                    match binding.node {
                        Some(node) => {
                            window.node = node;
                            true
                        }
                        None => false,
                    }
                }
                _ => false,
            }
        });
        let mut index = 0;
        self.bindings.retain(|binding| {
            let keep = used[index] || binding.park_start >= moment;
            index += 1;
            keep
        });
        bound
    }

    pub fn resolve(&mut self, pause: RolePause, node: Option<u16>, moment: u64) -> Option<u16> {
        let node = node.filter(|_| moment < pause.end);
        self.bindings.push(Binding {
            start: pause.start,
            park_start: pause.end,
            from: moment,
            node,
        });
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

    fn asked(pause: RolePause) -> Bound {
        Bound {
            ask: Some(pause),
            expired: Vec::new(),
        }
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
        assert_eq!(roles.bind(&mut early, None), Bound::default());
        assert_eq!(early.windows, windows[..1]);

        let mut started = standing(12, windows.clone());
        let pause = RolePause { start: 10, end: 60 };
        assert_eq!(roles.bind(&mut started, None), asked(pause));
        assert_eq!(started.windows, windows[..1]);

        let mut waiting = standing(13, windows.clone());
        assert_eq!(roles.bind(&mut waiting, Some(pause)), Bound::default());
        assert_eq!(waiting.windows, windows[..1]);
        assert_eq!(roles.resolve(pause, Some(2), 14), Some(2));

        let mut paused = standing(20, windows.clone());
        assert_eq!(roles.bind(&mut paused, None), Bound::default());
        assert_eq!(
            paused.windows,
            [
                windows[0],
                window(2, ProcessAction::Pause(50), 14, 60),
                window(2, park(), 60, 110),
            ]
        );

        let mut parked = standing(80, windows[2..].to_vec());
        assert_eq!(roles.bind(&mut parked, None), Bound::default());
        assert_eq!(parked.windows, [window(2, park(), 60, 110)]);
    }

    #[test]
    fn a_role_pause_answered_in_its_lead_starts_at_its_drawn_opening_with_its_drawn_length() {
        let mut roles = RoleWindows::new();
        let windows = vec![
            window(ROLE_NODE, ProcessAction::Pause(50), 10, 188),
            window(ROLE_NODE, park(), 188, 238),
        ];
        let pause = RolePause {
            start: 10,
            end: 188,
        };
        assert_eq!(
            roles.bind(&mut standing(10, windows.clone()), None),
            asked(pause)
        );
        assert_eq!(roles.resolve(pause, Some(1), 70), Some(1));
        let mut answered = standing(80, windows.clone());
        assert_eq!(roles.bind(&mut answered, None), Bound::default());
        assert_eq!(
            answered.windows,
            [
                window(1, ProcessAction::Pause(50), 138, 188),
                window(1, park(), 188, 238),
            ]
        );
        assert!(
            answered
                .steps(Some(5))
                .iter()
                .all(|step| !step.node(1).pause)
        );

        let mut late = RoleWindows::new();
        late.resolve(pause, Some(1), 150);
        let mut shortened = standing(150, windows);
        late.bind(&mut shortened, None);
        assert_eq!(
            shortened.windows[0],
            window(1, ProcessAction::Pause(50), 150, 188)
        );
    }

    #[test]
    fn a_role_pause_cut_after_its_lead_runs_from_its_drawn_opening_to_the_cut() {
        let mut roles = RoleWindows::new();
        let pause = RolePause {
            start: 10,
            end: 188,
        };
        assert_eq!(roles.resolve(pause, Some(1), 30), Some(1));
        let mut cut = standing(
            145,
            vec![window(ROLE_NODE, ProcessAction::Pause(12), 10, 150)],
        );
        assert_eq!(roles.bind(&mut cut, None), Bound::default());
        assert_eq!(cut.windows, [window(1, ProcessAction::Pause(12), 138, 150)]);
        let steps = cut.steps(Some(125));
        assert_eq!(
            steps
                .iter()
                .map(|step| (step.moment(), step.node(1).pause))
                .collect::<Vec<_>>(),
            [(138, true), (145, true)]
        );
    }

    #[test]
    fn a_resolved_pause_starts_at_its_resolution_so_catch_up_edges_before_it_see_no_pause() {
        let mut roles = RoleWindows::new();
        let pause = RolePause { start: 10, end: 60 };
        roles.resolve(pause, Some(1), 30);
        let mut answer = standing(
            40,
            vec![window(ROLE_NODE, ProcessAction::Pause(50), 10, 60)],
        );
        roles.bind(&mut answer, None);
        let steps = answer.steps(Some(5));
        assert!(
            steps
                .iter()
                .all(|step| step.moment() >= 30 || !step.node(1).pause)
        );
        assert!(steps.last().unwrap().node(1).pause);
    }

    #[test]
    fn an_unresolved_role_pause_and_its_park_stay_out_of_every_step() {
        let mut roles = RoleWindows::new();
        let windows = vec![
            window(ROLE_NODE, ProcessAction::Pause(50), 10, 60),
            window(ROLE_NODE, park(), 60, 110),
        ];
        let pause = RolePause { start: 10, end: 60 };
        assert_eq!(
            roles.bind(&mut standing(10, windows.clone()), None),
            asked(pause)
        );
        assert_eq!(roles.resolve(pause, None, 11), None);
        let mut later = standing(70, windows);
        assert_eq!(roles.bind(&mut later, None), Bound::default());
        assert!(later.windows.is_empty());
    }

    #[test]
    fn a_role_known_only_after_its_pause_ends_is_unresolved() {
        let mut roles = RoleWindows::new();
        let pause = RolePause { start: 10, end: 60 };
        assert_eq!(roles.resolve(pause, Some(1), 60), None);
        let mut parked = standing(61, vec![window(ROLE_NODE, park(), 60, 110)]);
        assert_eq!(roles.bind(&mut parked, None), Bound::default());
        assert!(parked.windows.is_empty());
    }

    #[test]
    fn a_catch_up_poll_settles_an_expired_pause_once_and_asks_for_the_current_one() {
        let mut roles = RoleWindows::new();
        let windows = vec![
            window(ROLE_NODE, ProcessAction::Pause(10), 10, 20),
            window(ROLE_NODE, ProcessAction::Pause(12), 20, 32),
        ];
        let first = RolePause { start: 10, end: 20 };
        let second = RolePause { start: 20, end: 32 };
        assert_eq!(
            roles.bind(&mut standing(25, windows.clone()), None),
            Bound {
                ask: Some(second),
                expired: vec![first],
            }
        );
        assert_eq!(
            roles.bind(&mut standing(26, windows), Some(second)),
            Bound::default()
        );
    }

    #[test]
    fn a_shortened_pause_keeps_its_answer_and_ends_where_it_now_ends() {
        let mut roles = RoleWindows::new();
        let pause = RolePause { start: 10, end: 60 };
        roles.resolve(pause, Some(0), 20);
        let mut cut = standing(
            30,
            vec![window(ROLE_NODE, ProcessAction::Pause(50), 10, 40)],
        );
        assert_eq!(roles.bind(&mut cut, None), Bound::default());
        assert_eq!(cut.windows, [window(0, ProcessAction::Pause(50), 20, 40)]);
        let shortened = standing(
            30,
            vec![window(ROLE_NODE, ProcessAction::Pause(50), 10, 40)],
        );
        assert_eq!(
            RoleWindows::current(&shortened, pause),
            Some(RolePause { start: 10, end: 40 })
        );
    }

    #[test]
    fn a_park_two_role_pauses_could_own_is_dropped() {
        let mut roles = RoleWindows::new();
        roles.resolve(RolePause { start: 10, end: 60 }, Some(0), 12);
        roles.resolve(RolePause { start: 20, end: 60 }, Some(1), 22);
        let mut answer = standing(
            61,
            vec![
                window(ROLE_NODE, ProcessAction::Pause(50), 10, 60),
                window(ROLE_NODE, ProcessAction::Pause(40), 20, 60),
                window(ROLE_NODE, park(), 60, 100),
            ],
        );
        roles.bind(&mut answer, None);
        assert!(
            answer
                .windows
                .iter()
                .all(|window| !matches!(window.action, ProcessAction::EventPark { .. }))
        );
    }

    #[test]
    fn a_park_whose_own_pause_expired_unseen_stays_off_the_node_another_pause_holds() {
        let mut roles = RoleWindows::new();
        roles.resolve(RolePause { start: 10, end: 60 }, Some(1), 12);
        let mut answer = standing(
            61,
            vec![
                window(ROLE_NODE, ProcessAction::Pause(50), 10, 60),
                window(ROLE_NODE, ProcessAction::Pause(10), 50, 60),
                window(ROLE_NODE, park(), 60, 100),
            ],
        );
        let bound = roles.bind(&mut answer, None);
        assert_eq!(bound.expired, [RolePause { start: 50, end: 60 }]);
        assert!(
            answer
                .windows
                .iter()
                .all(|window| !matches!(window.action, ProcessAction::EventPark { .. }))
        );
    }

    #[test]
    fn a_repeated_expired_window_is_settled_once() {
        let mut roles = RoleWindows::new();
        let pause = window(ROLE_NODE, ProcessAction::Pause(10), 10, 20);
        let bound = roles.bind(&mut standing(25, vec![pause, pause]), None);
        assert_eq!(bound.expired, [RolePause { start: 10, end: 20 }]);
        assert_eq!(
            roles.bind(&mut standing(26, vec![pause]), None),
            Bound::default()
        );
    }

    #[test]
    fn a_binding_is_dropped_once_no_window_of_its_action_stands() {
        let mut roles = RoleWindows::new();
        let pause = RolePause { start: 10, end: 60 };
        roles.resolve(pause, Some(0), 20);
        roles.bind(&mut standing(200, Vec::new()), None);
        let mut again = standing(
            200,
            vec![window(ROLE_NODE, ProcessAction::Pause(50), 10, 60)],
        );
        assert_eq!(
            roles.bind(&mut again, None),
            Bound {
                ask: None,
                expired: vec![pause],
            }
        );
    }
}
