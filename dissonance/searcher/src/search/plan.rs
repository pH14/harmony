// SPDX-License-Identifier: AGPL-3.0-or-later

pub trait Stage: Copy {
    type State: ?Sized;

    fn name(self) -> &'static str;

    fn objective(self, state: &Self::State, carried: Option<u16>) -> u16;

    fn done(self, state: &Self::State, objective: u16) -> bool;

    fn latches(self) -> bool {
        false
    }
}

pub type StageProgress = (u8, u16);

#[must_use]
pub fn locate<S: Stage>(
    plan: &[S],
    state: &S::State,
    parent: Option<StageProgress>,
) -> StageProgress {
    for (index, stage) in plan.iter().enumerate() {
        let index = u8::try_from(index).unwrap_or(u8::MAX);
        let carried = parent.and_then(|(stage, objective)| (stage == index).then_some(objective));
        if stage.latches()
            && (parent.is_some_and(|(parent_stage, _)| parent_stage > index)
                || later_fact_done(&plan[usize::from(index) + 1..], state))
        {
            continue;
        }
        let objective = stage.objective(state, carried);
        if !stage.done(state, objective) {
            return (index, objective);
        }
    }
    (
        u8::try_from(plan.len().saturating_sub(1)).unwrap_or(u8::MAX),
        0,
    )
}

fn later_fact_done<S: Stage>(later: &[S], state: &S::State) -> bool {
    later
        .iter()
        .any(|stage| !stage.latches() && stage.done(state, stage.objective(state, None)))
}

#[must_use]
pub fn stage_name<S: Stage>(plan: &[S], stage: u8) -> &'static str {
    plan.get(usize::from(stage))
        .map_or("unknown", |stage| stage.name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    struct Reach {
        target: u16,
        latch: bool,
    }

    impl Stage for Reach {
        type State = u16;

        fn name(self) -> &'static str {
            "reach"
        }

        fn objective(self, state: &u16, carried: Option<u16>) -> u16 {
            if *state == 0 {
                carried.unwrap_or(0)
            } else {
                *state
            }
        }

        fn done(self, state: &u16, objective: u16) -> bool {
            *state > 0 && objective >= self.target
        }

        fn latches(self) -> bool {
            self.latch
        }
    }

    const PLAN: [Reach; 3] = [
        Reach {
            target: 3,
            latch: false,
        },
        Reach {
            target: 6,
            latch: true,
        },
        Reach {
            target: 9,
            latch: false,
        },
    ];

    #[test]
    fn the_first_stage_not_done_names_the_progress() {
        assert_eq!(locate(&PLAN, &1, None), (0, 1));
        assert_eq!(locate(&PLAN, &4, None), (1, 4));
        assert_eq!(locate(&PLAN, &7, None), (2, 7));
        assert_eq!(locate(&PLAN, &12, None), (2, 0));
        assert_eq!(stage_name(&PLAN, 1), "reach");
        assert_eq!(stage_name(&PLAN, 9), "unknown");
    }

    #[derive(Clone, Copy)]
    struct Flag {
        bit: u16,
        latch: bool,
    }

    impl Stage for Flag {
        type State = u16;

        fn name(self) -> &'static str {
            "flag"
        }

        fn objective(self, state: &u16, _carried: Option<u16>) -> u16 {
            *state
        }

        fn done(self, state: &u16, _objective: u16) -> bool {
            state & self.bit != 0
        }

        fn latches(self) -> bool {
            self.latch
        }
    }

    #[test]
    fn a_later_stage_that_does_not_latch_marks_the_latching_stages_before_it_done() {
        let plan = [
            Flag {
                bit: 1,
                latch: false,
            },
            Flag {
                bit: 2,
                latch: true,
            },
            Flag {
                bit: 4,
                latch: false,
            },
            Flag {
                bit: 8,
                latch: true,
            },
        ];
        assert_eq!(locate(&plan, &0b0001, None), (1, 1));
        assert_eq!(locate(&plan, &0b0101, None), (3, 5));
        assert_eq!(locate(&plan, &0b0100, None), (0, 4));
    }

    #[test]
    fn a_stage_carries_its_parent_objective_and_a_latched_stage_stays_done() {
        assert_eq!(locate(&PLAN, &0, Some((0, 2))), (0, 2));
        assert_eq!(locate(&PLAN, &4, Some((2, 7))), (2, 4));
        assert_eq!(locate(&[] as &[Reach], &4, None), (0, 0));
    }
}
