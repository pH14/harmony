// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use fault_policy::{Action, Answer, ChoiceQuestion, EnvSpec, FaultPolicy, RecordedEnv};
use searcher::search::choice::{Choice, ChoicePlan, ChoiceProfile, Chooser};
use serde::{Deserialize, Serialize};

pub const GUEST_SITE: &str = "guest";

const SEED_MIX: u64 = 0x6775_6573_745f_6368;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct GuestChoices {
    pub contrary: u32,
    pub hold: u32,
    pub seed: u64,
    pub untried: Option<u32>,
}

impl GuestChoices {
    #[must_use]
    pub fn from_plan(plan: &ChoicePlan, index: usize, untried: Option<u32>) -> Self {
        let mut z =
            (plan.seed ^ SEED_MIX).wrapping_add((index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15));
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        Self {
            contrary: plan.profile.contrary,
            hold: plan.profile.hold,
            seed: z ^ (z >> 31),
            untried,
        }
    }

    fn plan(self) -> ChoicePlan {
        ChoicePlan {
            profile: ChoiceProfile::new(self.contrary, self.hold),
            redrawn: false,
            seed: self.seed,
            untried: self.untried,
            held: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct GuestAnswer {
    pub step: u64,
    pub choice: u64,
    #[serde(default)]
    pub label: Option<u16>,
}

#[must_use]
pub fn answers_through(answers: &[GuestAnswer], steps: usize) -> Vec<GuestAnswer> {
    answers
        .iter()
        .filter(|answer| answer.step <= steps as u64)
        .copied()
        .collect()
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GuestAnswers(BTreeMap<u64, EnvSpec>);

impl GuestAnswers {
    pub fn new(answers: &[GuestAnswer]) -> Result<Self, String> {
        let mut steps: BTreeMap<u64, BTreeMap<u64, Action>> = BTreeMap::new();
        for answer in answers {
            if answer.step == 0 {
                return Err("a guest answer names an action step, which starts at 1".to_owned());
            }
            let value = answer.label.map_or(Answer::Nominal, Answer::Label);
            if steps
                .entry(answer.step)
                .or_default()
                .insert(answer.choice, Action::Guest(value))
                .is_some()
            {
                return Err(format!(
                    "step {} answers guest choice {} twice",
                    answer.step, answer.choice
                ));
            }
        }
        Ok(Self(
            steps
                .into_iter()
                .map(|(step, overrides)| {
                    (
                        step,
                        EnvSpec::Recorded {
                            seed: 0,
                            policy: FaultPolicy::none(),
                            overrides,
                            standing: Vec::new(),
                            reseeds: BTreeMap::new(),
                            payloads: None,
                        },
                    )
                })
                .collect(),
        ))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn identity(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (step, spec) in &self.0 {
            let encoded = spec.encode();
            out.extend_from_slice(&step.to_le_bytes());
            out.extend_from_slice(&(encoded.len() as u64).to_le_bytes());
            out.extend_from_slice(&encoded);
        }
        out
    }

    #[must_use]
    pub fn at(&self, step: u64) -> Option<&EnvSpec> {
        self.0.get(&step)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GuestChoice {
    pub step: u64,
    pub choice: u64,
    pub moment: u64,
    pub clock: u64,
    pub site: String,
    pub options: u16,
    pub default: u16,
    pub taken: Choice,
    pub label: u16,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub answered: bool,
}

pub struct GuestChooser {
    step: u64,
    chooser: Option<Chooser>,
    answers: Option<RecordedEnv>,
    sites: BTreeMap<String, u16>,
    offered: Vec<u16>,
    records: Vec<GuestChoice>,
}

impl GuestChooser {
    #[must_use]
    pub fn new(step: u64, guest: Option<GuestChoices>, answers: Option<&EnvSpec>) -> Self {
        Self {
            step,
            chooser: guest.map(|guest| Chooser::new(&guest.plan())),
            answers: answers.map(EnvSpec::materialize),
            sites: BTreeMap::new(),
            offered: Vec::new(),
            records: Vec::new(),
        }
    }

    pub fn answer(&mut self, moment: u64, payload: &[u8]) -> Result<Answer, String> {
        let question = ChoiceQuestion::decode(payload)
            .map_err(|error| format!("guest choice question: {error}"))?;
        let choice = self.records.len() as u64;
        let next = u16::try_from(self.sites.len())
            .map_err(|_| "an action asked more than 65,536 guest choice sites".to_owned())?;
        let site = *self.sites.entry(question.site.clone()).or_insert(next);
        if self.offered.len() < usize::from(question.options) {
            self.offered = (0..question.options).collect();
        }
        let offered = &self.offered[..usize::from(question.options)];
        let taken = self.chooser.as_mut().map_or(Choice::Default, |chooser| {
            chooser.choose(site, offered, question.default)
        });
        let planned = taken.label(offered, question.default);
        let answered = self.answers.as_mut().and_then(|answers| {
            answers.set_moment(choice);
            answers.recorded(&question.point()).cloned()
        });
        let label = match &answered {
            Some(Answer::Label(label)) => *label,
            Some(_) => question.default,
            None => planned,
        };
        self.records.push(GuestChoice {
            step: self.step,
            choice,
            moment,
            clock: question.clock,
            site: question.site,
            options: question.options,
            default: question.default,
            taken,
            label,
            answered: answered.is_some(),
        });
        Ok(if label == question.default {
            Answer::Nominal
        } else {
            Answer::Label(label)
        })
    }

    #[must_use]
    pub fn into_records(self) -> Vec<GuestChoice> {
        self.records
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use searcher::search::choice::{CERTAIN, ChoicePolicy, ChoiceProfiles};

    fn question(site: &str, options: u16, default: u16, clock: u64) -> Vec<u8> {
        ChoiceQuestion {
            site: site.to_owned(),
            options,
            default,
            clock,
        }
        .encode()
        .unwrap()
    }

    fn plan(contrary: u32, hold: u32, untried: Option<u32>) -> ChoicePlan {
        let policy = ChoicePolicy {
            profiles: ChoiceProfiles::Fixed(ChoiceProfile::new(contrary, hold)),
            untried_first: true,
        };
        ChoicePlan::new(&policy, ChoiceProfile::default(), 99, untried, Vec::new())
    }

    fn labels(guest: Option<GuestChoices>, answers: Option<&EnvSpec>) -> Vec<u16> {
        let mut chooser = GuestChooser::new(3, guest, answers);
        for index in 0..64u64 {
            let site = if index % 2 == 0 {
                "register.op"
            } else {
                "register.key"
            };
            chooser
                .answer(1_000 + index, &question(site, 4, (index % 4) as u16, index))
                .unwrap();
        }
        chooser
            .into_records()
            .iter()
            .map(|record| record.label)
            .collect()
    }

    #[test]
    fn without_a_plan_every_choice_takes_the_guest_default() {
        let defaults: Vec<u16> = (0..64).map(|index| (index % 4) as u16).collect();
        assert_eq!(labels(None, None), defaults);
        let never = GuestChoices::from_plan(&plan(0, 0, None), 0, None);
        assert_eq!(labels(Some(never), None), defaults);
    }

    #[test]
    fn a_plan_regenerates_its_answers() {
        let guest = GuestChoices::from_plan(&plan(CERTAIN / 2, 0, None), 4, None);
        let first = labels(Some(guest), None);
        assert_eq!(first, labels(Some(guest), None));
        let defaults: Vec<u16> = (0..64).map(|index| (index % 4) as u16).collect();
        assert_ne!(first, defaults);
        let other = GuestChoices::from_plan(&plan(CERTAIN / 2, 0, None), 5, None);
        assert_ne!(other.seed, guest.seed);
        assert_eq!(other.untried, None);
    }

    #[test]
    fn a_recorded_answer_replaces_one_choice_and_leaves_the_others() {
        let guest = GuestChoices::from_plan(&plan(CERTAIN / 4, CERTAIN / 2, None), 0, None);
        let base = labels(Some(guest), None);
        let changed = (base[10] + 1) % 4;
        let answers = GuestAnswers::new(&[GuestAnswer {
            step: 3,
            choice: 10,
            label: Some(changed),
        }])
        .unwrap();
        let replaced = labels(Some(guest), answers.at(3));
        assert_eq!(replaced[..10], base[..10]);
        assert_eq!(replaced[10], changed);
        assert_eq!(replaced[11..], base[11..]);
    }

    #[test]
    fn a_nominal_answer_forces_the_default_and_an_unoffered_label_is_ignored() {
        let guest = GuestChoices::from_plan(&plan(CERTAIN, 0, None), 0, None);
        let answers = GuestAnswers::new(&[
            GuestAnswer {
                step: 3,
                choice: 0,
                label: None,
            },
            GuestAnswer {
                step: 3,
                choice: 1,
                label: Some(9),
            },
        ])
        .unwrap();
        let mut chooser = GuestChooser::new(3, Some(guest), answers.at(3));
        assert_eq!(
            chooser.answer(1, &question("a", 4, 2, 0)).unwrap(),
            Answer::Nominal
        );
        let unoffered = chooser.answer(2, &question("a", 4, 2, 1)).unwrap();
        assert_ne!(unoffered, Answer::Nominal);
        let records = chooser.into_records();
        assert!(records[0].answered);
        assert!(!records[1].answered);
        assert_eq!(records[1].taken, Choice::Option(records[1].label));
    }

    #[test]
    fn answers_are_refused_for_the_root_step_and_for_a_repeated_choice() {
        let root = GuestAnswer {
            step: 0,
            choice: 0,
            label: Some(1),
        };
        assert!(GuestAnswers::new(&[root]).is_err());
        let once = GuestAnswer {
            step: 1,
            choice: 0,
            label: Some(1),
        };
        assert!(GuestAnswers::new(&[once, once]).is_err());
    }

    #[test]
    fn malformed_questions_are_refused() {
        let mut chooser = GuestChooser::new(1, None, None);
        assert!(chooser.answer(1, &[1, 0]).is_err());
    }
}
