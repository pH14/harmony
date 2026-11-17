// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, num::NonZeroUsize};

use serde::{Deserialize, Serialize};

use crate::search::rand::RomuDuoJrRand;

pub const CERTAIN: u32 = 1 << 16;

pub const REDRAW_ODDS: usize = 8;

pub const CHOICE_POLICY_PREFIX: &str = "choices_v1:";

const PROFILE_SEED_MIX: u64 = 0x6368_6f69_6365_5f70;

const ANSWER_SEED_MIX: u64 = 0x6368_6f69_6365_5f61;

const CONTRARY_HALVINGS: usize = 6;

const HOLD_HALVINGS: usize = 9;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Choice {
    Ask,
    Default,
    Option(u16),
}

impl Choice {
    #[must_use]
    pub const fn is_deviation(self) -> bool {
        matches!(self, Self::Option(_))
    }

    #[must_use]
    pub fn label(self, offered: &[u16], default: u16) -> u16 {
        match self {
            Self::Option(label) if offered.contains(&label) => label,
            Self::Ask | Self::Default | Self::Option(_) => default,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChoiceSite {
    pub name: String,
    pub labels: Vec<String>,
}

impl ChoiceSite {
    #[must_use]
    pub fn new(name: &str, labels: impl IntoIterator<Item = String>) -> Self {
        Self {
            name: name.to_owned(),
            labels: labels.into_iter().collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ChoiceProfile {
    pub contrary: u32,
    pub hold: u32,
}

impl ChoiceProfile {
    #[must_use]
    pub const fn new(contrary: u32, hold: u32) -> Self {
        Self { contrary, hold }
    }

    pub fn draw(rand: &mut RomuDuoJrRand) -> Self {
        let contrary_halvings = 1 + below(rand, CONTRARY_HALVINGS);
        let hold_halvings = below(rand, HOLD_HALVINGS);
        Self {
            contrary: CERTAIN >> contrary_halvings,
            hold: CERTAIN - (CERTAIN >> hold_halvings),
        }
    }

    fn validate(self) -> Result<Self, Box<dyn Error>> {
        if self.contrary > CERTAIN || self.hold > CERTAIN {
            return Err("a choice probability exceeds one".into());
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChoiceProfiles {
    Inherited,
    Fixed(ChoiceProfile),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChoicePolicy {
    pub profiles: ChoiceProfiles,
    pub untried_first: bool,
}

impl Default for ChoicePolicy {
    fn default() -> Self {
        Self {
            profiles: ChoiceProfiles::Inherited,
            untried_first: true,
        }
    }
}

impl ChoicePolicy {
    #[must_use]
    pub fn identifier(&self) -> String {
        let profiles = match self.profiles {
            ChoiceProfiles::Inherited => "inherited".to_owned(),
            ChoiceProfiles::Fixed(profile) => {
                format!("fixed:{}:{}", profile.contrary, profile.hold)
            }
        };
        let first = if self.untried_first {
            "untried_first"
        } else {
            "profile_first"
        };
        format!("{CHOICE_POLICY_PREFIX}{profiles},{first}")
    }

    pub fn validate(self) -> Result<Self, Box<dyn Error>> {
        if let ChoiceProfiles::Fixed(profile) = self.profiles {
            profile.validate()?;
        }
        Ok(self)
    }

    #[must_use]
    pub fn root_profile(&self, campaign_seed: u64) -> ChoiceProfile {
        match self.profiles {
            ChoiceProfiles::Inherited => ChoiceProfile::draw(&mut RomuDuoJrRand::with_seed(
                campaign_seed ^ PROFILE_SEED_MIX,
            )),
            ChoiceProfiles::Fixed(profile) => profile,
        }
    }

    #[must_use]
    pub fn job_profile(&self, parent: ChoiceProfile, mutation_seed: u64) -> (ChoiceProfile, bool) {
        match self.profiles {
            ChoiceProfiles::Inherited => {
                let mut rand = RomuDuoJrRand::with_seed(mutation_seed ^ PROFILE_SEED_MIX);
                if below(&mut rand, REDRAW_ODDS) == 0 {
                    (ChoiceProfile::draw(&mut rand), true)
                } else {
                    (parent, false)
                }
            }
            ChoiceProfiles::Fixed(profile) => (profile, false),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChoicePlan {
    pub profile: ChoiceProfile,
    pub redrawn: bool,
    pub seed: u64,
    pub untried: Option<u32>,
    pub held: Vec<(u16, Choice)>,
}

impl ChoicePlan {
    #[must_use]
    pub fn new(
        policy: &ChoicePolicy,
        parent: ChoiceProfile,
        mutation_seed: u64,
        untried: Option<u32>,
        held: Vec<(u16, Choice)>,
    ) -> Self {
        let (profile, redrawn) = policy.job_profile(parent, mutation_seed);
        Self {
            profile,
            redrawn,
            seed: mutation_seed ^ ANSWER_SEED_MIX,
            untried,
            held,
        }
    }

    #[must_use]
    pub fn record(&self) -> ChoicePlanRecord {
        ChoicePlanRecord {
            profile: self.profile,
            redrawn: self.redrawn,
            untried: self.untried,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChoicePlanRecord {
    pub profile: ChoiceProfile,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redrawn: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untried: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChoiceRecord {
    pub site: u16,
    pub offered: Vec<u16>,
    pub default: u16,
    pub taken: Choice,
    pub resolved: u16,
}

impl ChoiceRecord {
    #[must_use]
    pub fn new(site: u16, offered: Vec<u16>, default: u16, taken: Choice) -> Self {
        let resolved = taken.label(&offered, default);
        Self {
            site,
            offered,
            default,
            taken,
            resolved,
        }
    }

    #[must_use]
    pub fn fell_back(&self) -> bool {
        matches!(self.taken, Choice::Option(label) if label != self.resolved)
    }
}

#[derive(Clone, Debug)]
pub struct Chooser {
    profile: ChoiceProfile,
    rand: RomuDuoJrRand,
    untried: Option<u32>,
    asked: bool,
    held: Vec<(u16, Choice)>,
}

impl Chooser {
    #[must_use]
    pub fn new(plan: &ChoicePlan) -> Self {
        Self {
            profile: plan.profile,
            rand: RomuDuoJrRand::with_seed(plan.seed),
            untried: plan.untried,
            asked: false,
            held: plan.held.clone(),
        }
    }

    pub fn choose(&mut self, site: u16, offered: &[u16], default: u16) -> Choice {
        let answer = self.answer(site, offered, default);
        match self.held.iter_mut().find(|(held, _)| *held == site) {
            Some(slot) => slot.1 = answer,
            None => self.held.push((site, answer)),
        }
        answer
    }

    fn answer(&mut self, site: u16, offered: &[u16], default: u16) -> Choice {
        let first = !self.asked;
        self.asked = true;
        if offered.len() < 2 {
            return Choice::Default;
        }
        let normal = |label: u16| {
            if label == default {
                Choice::Default
            } else {
                Choice::Option(label)
            }
        };
        if first
            && let Some(ordinal) = self.untried
            && let Some(label) = untried_order(offered, default).nth(ordinal as usize)
        {
            return normal(label);
        }
        if let Some(previous) = self
            .held
            .iter()
            .find(|(held, _)| *held == site)
            .map(|(_, answer)| *answer)
            && self.chance(self.profile.hold)
        {
            match previous {
                Choice::Ask | Choice::Default => return Choice::Default,
                Choice::Option(label) if offered.contains(&label) => {
                    return normal(label);
                }
                Choice::Option(_) => {}
            }
        }
        if self.chance(self.profile.contrary) {
            let others = offered.iter().filter(|label| **label != default).count();
            if others > 0 {
                let pick = below(&mut self.rand, others);
                if let Some(label) = offered
                    .iter()
                    .copied()
                    .filter(|label| *label != default)
                    .nth(pick)
                {
                    return Choice::Option(label);
                }
            }
        }
        Choice::Default
    }

    fn chance(&mut self, probability: u32) -> bool {
        if probability == 0 {
            return false;
        }
        (self.rand.next_u64() >> 48) < u64::from(probability)
    }
}

fn untried_order(offered: &[u16], default: u16) -> impl Iterator<Item = u16> + '_ {
    let mut rest: Vec<u16> = offered
        .iter()
        .copied()
        .filter(|label| *label != default)
        .collect();
    rest.sort_unstable();
    rest.dedup();
    offered
        .contains(&default)
        .then_some(default)
        .into_iter()
        .chain(rest)
}

#[must_use]
pub fn deviations<'a>(
    answers: impl IntoIterator<Item = (usize, &'a [(u16, Choice)])>,
) -> Vec<(usize, u16, u16)> {
    answers
        .into_iter()
        .flat_map(|(index, answers)| {
            answers
                .iter()
                .filter_map(move |(site, answer)| match answer {
                    Choice::Option(label) => Some((index, *site, *label)),
                    Choice::Ask | Choice::Default => None,
                })
        })
        .collect()
}

fn below(rand: &mut RomuDuoJrRand, bound: usize) -> usize {
    NonZeroUsize::new(bound).map_or(0, |bound| rand.below(bound))
}

#[cfg(test)]
mod tests {
    use super::{
        CERTAIN, Choice, ChoicePlan, ChoicePolicy, ChoiceProfile, ChoiceProfiles, ChoiceRecord,
        Chooser, deviations,
    };

    fn plan(contrary: u32, hold: u32, untried: Option<u32>) -> ChoicePlan {
        ChoicePlan {
            profile: ChoiceProfile::new(contrary, hold),
            redrawn: false,
            seed: 7,
            untried,
            held: Vec::new(),
        }
    }

    #[test]
    fn a_zero_rate_always_takes_the_default() {
        let mut chooser = Chooser::new(&plan(0, 0, None));
        for _ in 0..1000 {
            assert_eq!(chooser.choose(0, &[1, 3, 5], 3), Choice::Default);
        }
    }

    #[test]
    fn a_certain_rate_never_takes_the_default_and_covers_the_others() {
        let mut chooser = Chooser::new(&plan(CERTAIN, 0, None));
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..1000 {
            match chooser.choose(0, &[1, 3, 5], 3) {
                Choice::Option(label) => {
                    seen.insert(label);
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), [1, 5]);
    }

    #[test]
    fn the_contrary_rate_sets_the_deviation_share() {
        let mut chooser = Chooser::new(&plan(CERTAIN / 4, 0, None));
        let deviations = (0..40_000)
            .filter(|_| chooser.choose(0, &[0, 1, 2, 3], 0).is_deviation())
            .count();
        assert!((9_400..10_600).contains(&deviations), "{deviations}");
    }

    #[test]
    fn untried_first_walks_the_default_then_the_rest_in_label_order() {
        let offered = [6, 2, 4];
        let picks: Vec<Choice> = (0..4)
            .map(|ordinal| Chooser::new(&plan(0, 0, Some(ordinal))).choose(1, &offered, 4))
            .collect();
        assert_eq!(
            picks,
            [
                Choice::Default,
                Choice::Option(2),
                Choice::Option(6),
                Choice::Default
            ]
        );
    }

    #[test]
    fn untried_first_acts_only_on_the_first_choice() {
        let mut chooser = Chooser::new(&plan(0, 0, Some(1)));
        assert_eq!(chooser.choose(0, &[0, 1], 0), Choice::Option(1));
        assert_eq!(chooser.choose(0, &[0, 1], 0), Choice::Default);
    }

    #[test]
    fn an_unoffered_deviation_resolves_to_the_default_and_records_the_fallback() {
        let kept = ChoiceRecord::new(0, vec![1, 3, 5], 3, Choice::Option(5));
        assert_eq!((kept.resolved, kept.fell_back()), (5, false));
        let edited = ChoiceRecord::new(0, vec![1, 3], 3, Choice::Option(5));
        assert_eq!((edited.resolved, edited.fell_back()), (3, true));
        let default = ChoiceRecord::new(0, vec![1, 3], 3, Choice::Default);
        assert_eq!((default.resolved, default.fell_back()), (3, false));
    }

    #[test]
    fn a_held_site_repeats_its_answer_while_it_is_still_offered() {
        let mut chooser = Chooser::new(&ChoicePlan {
            held: vec![(2, Choice::Option(5))],
            ..plan(0, CERTAIN, None)
        });
        assert_eq!(chooser.choose(2, &[0, 5], 0), Choice::Option(5));
        assert_eq!(chooser.choose(2, &[0, 5], 0), Choice::Option(5));
        assert_eq!(chooser.choose(2, &[0, 4], 0), Choice::Default);
        assert_eq!(chooser.choose(3, &[0, 5], 0), Choice::Default);
    }

    #[test]
    fn a_label_equal_to_the_default_is_stored_as_the_default() {
        let mut chooser = Chooser::new(&ChoicePlan {
            held: vec![(0, Choice::Option(1))],
            ..plan(0, CERTAIN, None)
        });
        assert_eq!(chooser.choose(0, &[0, 1], 1), Choice::Default);
    }

    #[test]
    fn a_site_with_one_option_draws_nothing() {
        let mut once = Chooser::new(&plan(CERTAIN / 2, 0, None));
        let mut twice = once.clone();
        once.choose(0, &[3], 3);
        assert_eq!(once.choose(0, &[0, 1], 0), twice.choose(0, &[0, 1], 0));
    }

    #[test]
    fn an_answer_no_longer_offered_falls_back_to_the_default() {
        assert_eq!(Choice::Option(9).label(&[1, 2], 2), 2);
        assert_eq!(Choice::Option(1).label(&[1, 2], 2), 1);
        assert_eq!(Choice::Default.label(&[1, 2], 1), 1);
    }

    #[test]
    fn a_run_with_no_deviation_lists_none() {
        let tape: [&[(u16, Choice)]; 2] = [&[(0, Choice::Default)], &[(0, Choice::Default)]];
        assert!(deviations(tape.into_iter().enumerate()).is_empty());
        let tape: [&[(u16, Choice)]; 2] = [&[(0, Choice::Default)], &[(1, Choice::Option(4))]];
        assert_eq!(deviations(tape.into_iter().enumerate()), [(1, 1, 4)]);
    }

    #[test]
    fn an_inherited_profile_is_redrawn_about_one_time_in_eight() {
        let policy = ChoicePolicy::default();
        let parent = ChoiceProfile::new(1, 1);
        let redrawn = (0..80_000_u64)
            .filter(|seed| policy.job_profile(parent, *seed).1)
            .count();
        assert!((9_400..10_600).contains(&redrawn), "{redrawn}");
        assert!(
            (0..64_u64)
                .filter(|seed| !policy.job_profile(parent, *seed).1)
                .all(|seed| policy.job_profile(parent, seed).0 == parent)
        );
    }

    #[test]
    fn a_fixed_profile_is_never_redrawn() {
        let fixed = ChoiceProfile::new(CERTAIN / 8, CERTAIN / 2);
        let policy = ChoicePolicy {
            profiles: ChoiceProfiles::Fixed(fixed),
            untried_first: false,
        };
        assert_eq!(policy.root_profile(3), fixed);
        assert!(
            (0..1000)
                .all(|seed| policy.job_profile(ChoiceProfile::default(), seed) == (fixed, false))
        );
    }

    #[test]
    fn drawn_profiles_span_the_declared_ranges() {
        let mut rand = crate::search::rand::RomuDuoJrRand::with_seed(11);
        let profiles: Vec<ChoiceProfile> =
            (0..4096).map(|_| ChoiceProfile::draw(&mut rand)).collect();
        let contrary: std::collections::BTreeSet<u32> =
            profiles.iter().map(|p| p.contrary).collect();
        let hold: std::collections::BTreeSet<u32> = profiles.iter().map(|p| p.hold).collect();
        assert_eq!(contrary.len(), 6);
        assert_eq!(contrary.first(), Some(&(CERTAIN >> 6)));
        assert_eq!(contrary.last(), Some(&(CERTAIN >> 1)));
        assert_eq!(hold.len(), 9);
        assert_eq!(hold.first(), Some(&0));
        assert_eq!(hold.last(), Some(&(CERTAIN - (CERTAIN >> 8))));
    }

    #[test]
    fn the_policy_identifier_names_the_arm() {
        assert_eq!(
            ChoicePolicy::default().identifier(),
            "choices_v1:inherited,untried_first"
        );
        let fixed = ChoicePolicy {
            profiles: ChoiceProfiles::Fixed(ChoiceProfile::new(0, 0)),
            untried_first: false,
        };
        assert_eq!(fixed.identifier(), "choices_v1:fixed:0:0,profile_first");
    }
}
