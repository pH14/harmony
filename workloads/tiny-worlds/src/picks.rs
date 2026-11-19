// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use searcher::search::{
    choice::{Choice, ChoicePolicy, ChoiceSite},
    rand::RomuDuoJrRand,
};
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;

pub const LABELS: u8 = 8;
pub const ASK: u8 = u8::MAX;
pub const DEFAULT: u8 = u8::MAX - 1;
const DEAD_PLACE: u16 = 64;
const PICK_SEED_MIX: u64 = 0x7069_636b_735f_6c61;
const ORDER_SEED_MIX: u64 = 0x7069_636b_735f_6f72;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Draw {
    Flat,
    Choices(ChoicePolicy),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub depth: u8,
    pub seed: u64,
    pub default_right: u8,
    pub draw: Draw,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub depth: u8,
    pub dead: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pick {
    pub offered: Vec<u16>,
    pub right: u16,
    pub default: u16,
}

fn below(rand: &mut RomuDuoJrRand, n: usize) -> usize {
    rand.below(NonZeroUsize::new(n).expect("nonzero range"))
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=16).contains(&self.depth) || self.default_right > self.depth {
            return Err("picks needs depth 1..=16 and default_right <= depth".into());
        }
        if let Draw::Choices(policy) = self.draw {
            policy.validate().map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    pub fn choices(&self) -> Option<ChoicePolicy> {
        match self.draw {
            Draw::Flat => None,
            Draw::Choices(policy) => Some(policy),
        }
    }
    pub fn sites(&self) -> Vec<ChoiceSite> {
        (0..self.depth)
            .map(|depth| {
                ChoiceSite::new(
                    &format!("pick-{depth}"),
                    (0..LABELS).map(|label| label.to_string()),
                )
            })
            .collect()
    }
    fn default_is_right(&self, depth: u8) -> bool {
        let mut order: Vec<u8> = (0..self.depth).collect();
        let mut rand = RomuDuoJrRand::with_seed(self.seed ^ ORDER_SEED_MIX);
        for i in (1..order.len()).rev() {
            order.swap(i, below(&mut rand, i + 1));
        }
        order[..usize::from(self.default_right)].contains(&depth)
    }
    pub fn pick(&self, depth: u8) -> Pick {
        let mut rand = RomuDuoJrRand::with_seed(
            self.seed ^ PICK_SEED_MIX ^ u64::from(depth).wrapping_mul(0x9e37_79b9_7f4a_7c15),
        );
        let count = 2 + below(&mut rand, 5);
        let mut labels: Vec<u16> = (0..u16::from(LABELS)).collect();
        for i in 0..count {
            let j = i + below(&mut rand, labels.len() - i);
            labels.swap(i, j);
        }
        let right = labels[0];
        let wrong = labels[1 + below(&mut rand, count - 1)];
        let default = if self.default_is_right(depth) {
            right
        } else {
            wrong
        };
        let mut offered = labels[..count].to_vec();
        offered.sort_unstable();
        Pick {
            offered,
            right,
            default,
        }
    }
    pub fn initial(&self) -> State {
        State {
            depth: 0,
            dead: false,
        }
    }
    pub fn state_is_bounded(&self, s: State) -> bool {
        s.depth <= self.depth && !(s.dead && s.depth == self.depth)
    }
    pub fn goal(&self, s: State) -> bool {
        self.state_is_bounded(s) && s.depth == self.depth
    }
    pub fn encode(answer: Choice) -> u8 {
        match answer {
            Choice::Ask => ASK,
            Choice::Default => DEFAULT,
            Choice::Option(label) => u8::try_from(label).unwrap_or(ASK),
        }
    }
    pub fn decode(action: u8) -> Choice {
        match action {
            ASK => Choice::Ask,
            DEFAULT => Choice::Default,
            label => Choice::Option(u16::from(label)),
        }
    }
    pub fn label(&self, s: State, action: u8) -> Option<u16> {
        let pick = self.pick(s.depth);
        match (self.draw, Self::decode(action)) {
            (_, Choice::Option(label)) if label >= u16::from(LABELS) => None,
            (Draw::Flat, Choice::Option(label)) => pick.offered.contains(&label).then_some(label),
            (Draw::Flat, Choice::Ask | Choice::Default) => None,
            (Draw::Choices(_), answer) => Some(answer.label(&pick.offered, pick.default)),
        }
    }
    pub fn step(&self, s: State, action: u8) -> State {
        if s.dead || self.goal(s) || !self.state_is_bounded(s) {
            return s;
        }
        match self.label(s, action) {
            None => s,
            Some(label) if label == self.pick(s.depth).right => State {
                depth: s.depth + 1,
                ..s
            },
            Some(_) => State { dead: true, ..s },
        }
    }
    pub fn key(&self, s: State) -> Key {
        Key {
            stock: 0,
            place: if s.dead {
                DEAD_PLACE
            } else {
                u16::from(s.depth)
            },
            context: 0,
            charge: 0,
            health: 0,
            goal: self.goal(s),
            tier: u16::from(s.depth),
        }
    }
    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        let route: Vec<u8> = (0..self.depth)
            .map(|depth| u8::try_from(self.pick(depth).right).expect("label fits"))
            .collect();
        let end = route.iter().fold(self.initial(), |s, &label| {
            self.step(
                s,
                match self.draw {
                    Draw::Flat => label,
                    Draw::Choices(_) => {
                        if self.pick(s.depth).default == u16::from(label) {
                            DEFAULT
                        } else {
                            label
                        }
                    }
                },
            )
        });
        Ok(self.goal(end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use searcher::search::choice::Chooser;

    fn config(draw: Draw) -> Config {
        Config {
            depth: 12,
            seed: 7,
            default_right: 8,
            draw,
        }
    }

    #[test]
    fn each_depth_is_its_own_choice_site() {
        let sites = config(Draw::Choices(ChoicePolicy::default())).sites();
        let names: std::collections::BTreeSet<_> = sites.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.len(), 12);
        assert!(sites.iter().all(|s| s.labels.len() == usize::from(LABELS)));
    }

    #[test]
    fn layout_offers_two_to_six_labels_with_the_declared_default_share() {
        for seed in 0..64 {
            let w = Config {
                seed,
                ..config(Draw::Flat)
            };
            assert!(w.reachable().unwrap());
            let mut right = 0;
            for depth in 0..w.depth {
                let pick = w.pick(depth);
                assert!((2..=6).contains(&pick.offered.len()));
                assert!(pick.offered.contains(&pick.right));
                assert!(pick.offered.contains(&pick.default));
                right += usize::from(pick.default == pick.right);
            }
            assert_eq!(right, 8);
        }
    }

    #[test]
    fn a_wrong_pick_ends_the_run_and_an_unoffered_flat_label_waits() {
        let w = config(Draw::Flat);
        let pick = w.pick(0);
        let wrong = *pick.offered.iter().find(|l| **l != pick.right).unwrap();
        let dead = w.step(w.initial(), u8::try_from(wrong).unwrap());
        assert!(dead.dead);
        assert_eq!(w.step(dead, u8::try_from(pick.right).unwrap()), dead);
        let unoffered = (0..u16::from(LABELS))
            .find(|l| !pick.offered.contains(l))
            .unwrap();
        assert_eq!(
            w.step(w.initial(), u8::try_from(unoffered).unwrap()),
            w.initial()
        );
        assert_eq!(w.step(w.initial(), DEFAULT), w.initial());
        assert_eq!(w.key(dead).place, DEAD_PLACE);
    }

    #[test]
    fn choices_resolve_default_and_fall_back_from_an_unoffered_label() {
        let w = config(Draw::Choices(ChoicePolicy::default()));
        let pick = w.pick(0);
        let after_default = w.step(w.initial(), DEFAULT);
        assert_eq!(after_default.dead, pick.default != pick.right);
        let unoffered = (0..u16::from(LABELS))
            .find(|l| !pick.offered.contains(l))
            .unwrap();
        assert_eq!(
            w.step(w.initial(), u8::try_from(unoffered).unwrap()),
            after_default
        );
        for answer in [Choice::Ask, Choice::Default, Choice::Option(3)] {
            assert_eq!(Config::decode(Config::encode(answer)), answer);
        }
    }

    fn workload(draw: Draw) -> crate::Workload {
        crate::Workload {
            keying: crate::Hand::<false>,
            config: crate::worlds::World::Picks(Config {
                depth: 6,
                default_right: 4,
                ..config(draw)
            }),
            broken: false,
            scale: None,
        }
    }

    #[test]
    fn campaign_replays_with_and_without_choices() {
        let fixed = ChoicePolicy {
            profiles: searcher::search::choice::ChoiceProfiles::Fixed(
                searcher::search::choice::ChoiceProfile::new(1 << 13, 1 << 12),
            ),
            untried_first: false,
        };
        for draw in [
            Draw::Flat,
            Draw::Choices(ChoicePolicy::default()),
            Draw::Choices(fixed),
        ] {
            for seed in [crate::test_seed(), 1, 2] {
                let report = crate::run(&workload(draw), seed, 4000, true).unwrap();
                assert_eq!(report["verified"], true);
            }
            let spliced = crate::SearchSettings {
                mixture: Some(
                    searcher::search::draw::draw_mixture_from_identifier("energy_splice:6")
                        .unwrap(),
                ),
                stop_on_objective: Some(false),
            };
            for seed in [crate::test_seed(), 3] {
                let report =
                    crate::campaign(&workload(draw), seed, 6000, true, 3, spliced).unwrap();
                assert_eq!(report["verified"], true);
            }
        }
    }

    #[test]
    fn the_stream_names_the_policy_and_each_job_plan_only_with_choices() {
        use searcher::search::campaign::{
            CampaignExecutionOptions, CampaignOrigin, CampaignStreamHeader, CampaignStreamRecord,
            run_campaign_checkpointed_with_options,
        };
        for (draw, expected) in [
            (Draw::Flat, None),
            (
                Draw::Choices(ChoicePolicy::default()),
                Some("choices_v1:inherited,untried_first"),
            ),
        ] {
            let w = workload(draw);
            let config = crate::campaign_config(&w, 9, 3000, 1);
            let mut stream = crate::BoundedStream(Vec::new());
            run_campaign_checkpointed_with_options(
                &w,
                &config,
                &CampaignOrigin::Genesis,
                &mut stream,
                None,
                CampaignExecutionOptions {
                    work_budget: Some(3000),
                    ..Default::default()
                },
            )
            .unwrap();
            let header_line = stream.0.split(|&c| c == b'\n').next().unwrap();
            let header: CampaignStreamHeader<serde_json::Value> =
                serde_json::from_slice(header_line).unwrap();
            assert_eq!(header.choice_policy.as_deref(), expected);
            let plans: Vec<_> = crate::stream_records(&stream.0)
                .unwrap()
                .into_iter()
                .filter_map(|record| match record {
                    CampaignStreamRecord::Job(job) => Some(job.choices),
                    _ => None,
                })
                .collect();
            assert!(!plans.is_empty());
            if expected.is_some() {
                assert!(plans.iter().all(Option::is_some));
                assert!(plans.iter().flatten().any(|plan| plan.untried == Some(0)));
                assert!(plans.iter().flatten().any(|plan| plan.untried > Some(1)));
            } else {
                assert!(plans.iter().all(Option::is_none));
            }
        }
    }

    #[test]
    fn root_jobs_with_choices_reach_the_goal_and_flat_draws_are_censored_sooner() {
        let choices = crate::root_jobs(
            &workload(Draw::Choices(ChoicePolicy::default())),
            3,
            200_000,
        )
        .unwrap();
        assert_eq!(choices["censored"], false, "{choices}");
        let flat = crate::root_jobs(&workload(Draw::Flat), 3, 4).unwrap();
        assert_eq!(flat["jobs"], 4);
        assert_eq!(
            crate::root_jobs(
                &workload(Draw::Choices(ChoicePolicy::default())),
                3,
                200_000
            )
            .unwrap(),
            choices
        );
    }

    #[test]
    fn untried_first_takes_each_first_pick_once_from_the_root() {
        use searcher::search::campaign::TargetExecution;
        let policy = ChoicePolicy {
            profiles: searcher::search::choice::ChoiceProfiles::Fixed(
                searcher::search::choice::ChoiceProfile::new(0, 0),
            ),
            untried_first: true,
        };
        let w = workload(Draw::Choices(policy));
        let crate::worlds::World::Picks(picks) = &w.config else {
            unreachable!()
        };
        let pick = picks.pick(0);
        let target = w.new_target().unwrap();
        let mut taken = std::collections::BTreeSet::new();
        for ordinal in 0..pick.offered.len() + 2 {
            let plan = searcher::search::choice::ChoicePlan::new(
                &policy,
                policy.root_profile(1),
                u64::try_from(ordinal).unwrap(),
                Some(u32::try_from(ordinal).unwrap()),
                Vec::new(),
                Vec::new(),
            );
            let mut chooser = Chooser::new(&plan);
            let action = w.resolve_action(&target, &ASK, &mut chooser).unwrap();
            let label = picks.label(picks.initial(), action).unwrap();
            if ordinal < pick.offered.len() {
                assert!(taken.insert(label));
            } else {
                assert_eq!(action, DEFAULT);
            }
        }
        assert_eq!(taken.into_iter().collect::<Vec<_>>(), pick.offered);
    }

    #[test]
    fn strict_schema_and_bounds() {
        let w = config(Draw::Flat);
        assert!(Config { depth: 0, ..w }.validate().is_err());
        assert!(Config { depth: 17, ..w }.validate().is_err());
        assert!(
            Config {
                default_right: 13,
                ..w
            }
            .validate()
            .is_err()
        );
        let mut v = serde_json::to_value(w).unwrap();
        v["unused"] = true.into();
        assert!(serde_json::from_value::<Config>(v).is_err());
        let choices = serde_json::to_value(config(Draw::Choices(ChoicePolicy::default()))).unwrap();
        assert_eq!(
            choices["draw"],
            serde_json::json!({"choices": {"profiles": "inherited", "untried_first": true}})
        );
    }
}
