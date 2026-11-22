// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    error::Error,
    num::{NonZeroU64, NonZeroUsize},
};

use serde::{Deserialize, Serialize};

use crate::search::rand::{RomuDuoJrRand, splitmix64};

pub const CERTAIN: u32 = 1 << 16;

pub const REDRAW_ODDS: usize = 8;

pub const CHOICE_POLICY_PREFIX: &str = "choices_v1:";

const PROFILE_SEED_MIX: u64 = 0x6368_6f69_6365_5f70;

const ANSWER_SEED_MIX: u64 = 0x6368_6f69_6365_5f61;

const PERMUTED_SEED_MIX: u64 = 0x6368_6f69_6365_5f77;

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
    pub contrary: Option<u32>,
    pub no_default: bool,
}

impl ChoiceSite {
    #[must_use]
    pub fn new(name: &str, labels: impl IntoIterator<Item = String>) -> Self {
        Self {
            name: name.to_owned(),
            labels: labels.into_iter().collect(),
            contrary: None,
            no_default: false,
        }
    }

    #[must_use]
    pub fn without_default(self) -> Self {
        Self {
            no_default: true,
            ..self
        }
    }

    #[must_use]
    pub fn with_contrary(self, contrary: u32) -> Self {
        Self {
            contrary: Some(contrary),
            ..self
        }
    }

    pub fn validate_all(sites: &[Self]) -> Result<(), Box<dyn Error>> {
        match sites
            .iter()
            .find(|site| site.contrary.is_some_and(|contrary| contrary > CERTAIN))
        {
            Some(site) => {
                Err(format!("choice site {} declares a rate above one", site.name).into())
            }
            None => Ok(()),
        }
    }

    #[must_use]
    pub fn declared_contrary(sites: &[Self]) -> Vec<(u16, u32)> {
        sites
            .iter()
            .enumerate()
            .filter_map(|(index, site)| Some((u16::try_from(index).ok()?, site.contrary?)))
            .collect()
    }

    #[must_use]
    pub fn sites_without_default(sites: &[Self]) -> Vec<u16> {
        sites
            .iter()
            .enumerate()
            .filter(|(_, site)| site.no_default)
            .filter_map(|(index, _)| u16::try_from(index).ok())
            .collect()
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
    pub untried_site: u16,
    pub held: Vec<(u16, Choice)>,
    pub declared: Vec<(u16, u32)>,
    pub permuted: Vec<(u16, u64)>,
}

impl ChoicePlan {
    #[must_use]
    pub fn new(
        policy: &ChoicePolicy,
        parent: ChoiceProfile,
        mutation_seed: u64,
        untried: Option<u32>,
        held: Vec<(u16, Choice)>,
        declared: Vec<(u16, u32)>,
    ) -> Self {
        let (profile, redrawn) = policy.job_profile(parent, mutation_seed);
        Self {
            profile,
            redrawn,
            seed: mutation_seed ^ ANSWER_SEED_MIX,
            untried,
            untried_site: 0,
            held,
            declared,
            permuted: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_permuted_sites(self, sites: &[u16], campaign_seed: u64, parent_id: u64) -> Self {
        let mut state = campaign_seed ^ PERMUTED_SEED_MIX;
        let mut parent = splitmix64(&mut state) ^ parent_id;
        let parent = splitmix64(&mut parent);
        Self {
            permuted: sites
                .iter()
                .map(|site| (*site, parent ^ u64::from(*site)))
                .collect(),
            ..self
        }
    }

    #[must_use]
    pub fn record(&self) -> ChoicePlanRecord {
        ChoicePlanRecord {
            profile: self.profile,
            redrawn: self.redrawn,
            untried: self.untried,
            untried_site: self.untried_site,
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
    #[serde(default, skip_serializing_if = "is_first_site")]
    pub untried_site: u16,
}

fn is_first_site(site: &u16) -> bool {
    *site == 0
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
    declared: Vec<(u16, u32)>,
    permuted: Vec<(u16, u64)>,
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
            declared: plan.declared.clone(),
            permuted: plan.permuted.clone(),
        }
    }

    #[must_use]
    pub fn contrary(&self, site: u16) -> u32 {
        self.declared_rate(site).unwrap_or(self.profile.contrary)
    }

    fn declared_rate(&self, site: u16) -> Option<u32> {
        self.declared
            .iter()
            .find(|(declared, _)| *declared == site)
            .map(|(_, contrary)| *contrary)
    }

    pub fn choose(&mut self, site: u16, offered: &[u16], default: u16) -> Choice {
        let weighted: Vec<(u16, u32)> = offered.iter().map(|label| (*label, 1)).collect();
        self.ask(site, &weighted, default, Draw::Contrary)
    }

    pub fn choose_weighted(&mut self, site: u16, offered: &[(u16, u32)], default: u16) -> Choice {
        self.ask(site, offered, default, Draw::Contrary)
    }

    pub fn draw(&mut self, site: u16, offered: &[(u16, u32)], default: u16) -> Choice {
        self.ask(site, offered, default, Draw::Weighted)
    }

    fn ask(&mut self, site: u16, offered: &[(u16, u32)], default: u16, draw: Draw) -> Choice {
        if let Some(rate) = self.declared_rate(site) {
            return self.declared_answer(rate, offered, default);
        }
        let answer = self.answer(site, offered, default, draw);
        match self.held.iter_mut().find(|(held, _)| *held == site) {
            Some(slot) => slot.1 = answer,
            None => self.held.push((site, answer)),
        }
        answer
    }

    fn answer(&mut self, site: u16, offered: &[(u16, u32)], default: u16, draw: Draw) -> Choice {
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
            && let Some(label) = self.untried_label(site, offered, default, ordinal as usize)
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
                Choice::Option(label) if offered.iter().any(|(offer, _)| *offer == label) => {
                    return normal(label);
                }
                Choice::Option(_) => {}
            }
        }
        match draw {
            Draw::Contrary => {
                if self.chance(self.contrary(site))
                    && let Some(label) = weighted_pick(
                        &mut self.rand,
                        offered
                            .iter()
                            .copied()
                            .filter(|(label, _)| *label != default),
                    )
                {
                    return Choice::Option(label);
                }
                Choice::Default
            }
            Draw::Weighted => weighted_pick(&mut self.rand, offered.iter().copied())
                .map_or(Choice::Default, normal),
        }
    }

    fn untried_label(
        &self,
        site: u16,
        offered: &[(u16, u32)],
        default: u16,
        ordinal: usize,
    ) -> Option<u16> {
        match self.permuted.iter().find(|(permuted, _)| *permuted == site) {
            Some((_, seed)) => permuted_order(offered, *seed).get(ordinal).copied(),
            None => untried_order(offered, default).nth(ordinal),
        }
    }

    fn declared_answer(&mut self, rate: u32, offered: &[(u16, u32)], default: u16) -> Choice {
        if offered.len() < 2 || !self.chance(rate) {
            return Choice::Default;
        }
        weighted_pick(
            &mut self.rand,
            offered
                .iter()
                .filter(|(label, _)| *label != default)
                .map(|(label, _)| (*label, 1)),
        )
        .map_or(Choice::Default, Choice::Option)
    }

    fn chance(&mut self, probability: u32) -> bool {
        if probability == 0 {
            return false;
        }
        (self.rand.next_u64() >> 48) < u64::from(probability)
    }
}

#[derive(Clone, Copy)]
enum Draw {
    Contrary,
    Weighted,
}

fn weighted_pick(
    rand: &mut RomuDuoJrRand,
    options: impl Iterator<Item = (u16, u32)> + Clone,
) -> Option<u16> {
    let total: u64 = options.clone().map(|(_, weight)| u64::from(weight)).sum();
    let mut point = rand.below_u64(NonZeroU64::new(total)?);
    for (label, weight) in options {
        if point < u64::from(weight) {
            return Some(label);
        }
        point -= u64::from(weight);
    }
    None
}

fn untried_order(offered: &[(u16, u32)], default: u16) -> impl Iterator<Item = u16> + '_ {
    let mut rest: Vec<u16> = offered
        .iter()
        .map(|(label, _)| *label)
        .filter(|label| *label != default)
        .collect();
    rest.sort_unstable();
    rest.dedup();
    offered
        .iter()
        .any(|(label, _)| *label == default)
        .then_some(default)
        .into_iter()
        .chain(rest)
}

fn permuted_order(offered: &[(u16, u32)], seed: u64) -> Vec<u16> {
    let mut labels: Vec<u16> = offered.iter().map(|(label, _)| *label).collect();
    labels.sort_unstable();
    labels.dedup();
    let mut rand = RomuDuoJrRand::with_seed(seed);
    for last in (1..labels.len()).rev() {
        labels.swap(last, below(&mut rand, last + 1));
    }
    labels
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
        ChoiceSite, Chooser, deviations,
    };

    fn plan(contrary: u32, hold: u32, untried: Option<u32>) -> ChoicePlan {
        ChoicePlan {
            profile: ChoiceProfile::new(contrary, hold),
            redrawn: false,
            seed: 7,
            untried,
            untried_site: 0,
            held: Vec::new(),
            declared: Vec::new(),
            permuted: Vec::new(),
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
    fn a_declared_rate_replaces_the_profile_contrary_at_its_site_alone() {
        let sites = [
            ChoiceSite::new("chord", ["a", "b"].map(str::to_owned)),
            ChoiceSite::new("move", ["chord", "x", "y"].map(str::to_owned))
                .with_contrary(CERTAIN / 12),
        ];
        assert_eq!(ChoiceSite::declared_contrary(&sites), [(1, CERTAIN / 12)]);
        let mut chooser = Chooser::new(&ChoicePlan {
            declared: ChoiceSite::declared_contrary(&sites),
            ..plan(CERTAIN / 2, 0, None)
        });
        assert_eq!(
            (chooser.contrary(0), chooser.contrary(1)),
            (CERTAIN / 2, CERTAIN / 12)
        );
        let mut moves = 0;
        let mut chords = 0;
        for _ in 0..48_000 {
            moves += usize::from(chooser.choose(1, &[0, 1, 2], 0).is_deviation());
            chords += usize::from(chooser.choose(0, &[0, 1], 0).is_deviation());
        }
        assert!((3_700..4_300).contains(&moves), "{moves}");
        assert!((23_400..24_600).contains(&chords), "{chords}");
        let mut zero = Chooser::new(&ChoicePlan {
            declared: vec![(1, 0)],
            ..plan(CERTAIN, 0, None)
        });
        assert!((0..1000).all(|_| zero.choose(1, &[0, 1, 2], 0) == Choice::Default));
    }

    #[test]
    fn a_declared_site_ignores_hold_and_untried_first_and_leaves_them_to_other_sites() {
        let mut chooser = Chooser::new(&ChoicePlan {
            held: vec![(1, Choice::Option(2))],
            declared: vec![(1, 0)],
            ..plan(0, CERTAIN, Some(1))
        });
        assert_eq!(chooser.choose(1, &[0, 1, 2], 0), Choice::Default);
        assert_eq!(chooser.choose(1, &[0, 1, 2], 0), Choice::Default);
        assert_eq!(chooser.choose(0, &[0, 1], 0), Choice::Option(1));
        let mut weighted = Chooser::new(&ChoicePlan {
            declared: vec![(1, CERTAIN)],
            ..plan(0, CERTAIN, None)
        });
        let mut taken = [0_u32; 3];
        for _ in 0..30_000 {
            if let Choice::Option(label) = weighted.choose_weighted(1, &[(0, 1), (1, 1), (2, 9)], 0)
            {
                taken[usize::from(label)] += 1;
            }
        }
        assert_eq!(taken[0], 0);
        assert!((14_000..16_000).contains(&taken[1]), "{taken:?}");
    }

    #[test]
    fn a_rate_above_one_is_refused() {
        let site = |contrary| {
            ChoiceSite::new("move", ["a", "b"].map(str::to_owned)).with_contrary(contrary)
        };
        assert!(ChoiceSite::validate_all(&[site(CERTAIN)]).is_ok());
        assert!(ChoiceSite::validate_all(&[site(CERTAIN + 1)]).is_err());
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
    fn untried_first_at_a_site_without_a_default_walks_one_permutation_per_parent() {
        let sites = [
            ChoiceSite::new("guest", Vec::new()),
            ChoiceSite::new("node", ["0", "1", "2"].map(str::to_owned)).without_default(),
        ];
        assert_eq!(ChoiceSite::sites_without_default(&sites), [1]);
        let offered = [(0, 1), (1, 1), (2, 1)];
        let first = |parent: u64, ordinal: u32| {
            let plan = plan(0, 0, Some(ordinal)).with_permuted_sites(
                &ChoiceSite::sites_without_default(&sites),
                3,
                parent,
            );
            let mut chooser = Chooser::new(&plan);
            chooser.draw(1, &offered, 0)
        };
        for parent in 0..16 {
            let mut walked: Vec<u16> = (0..3)
                .map(|ordinal| first(parent, ordinal).label(&[0, 1, 2], 0))
                .collect();
            walked.sort_unstable();
            assert_eq!(walked, [0, 1, 2], "parent {parent}");
        }
        let starts: std::collections::BTreeSet<u16> = (0..64)
            .map(|parent| first(parent, 0).label(&[0, 1, 2], 0))
            .collect();
        assert_eq!(starts.into_iter().collect::<Vec<_>>(), [0, 1, 2]);
        let defaulted = plan(0, 0, Some(0)).with_permuted_sites(&[1], 3, 5);
        assert_eq!(
            Chooser::new(&defaulted).choose(0, &[4, 6], 6),
            Choice::Default
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
    fn a_weighted_deviation_follows_the_weights_and_never_takes_a_zero_weight() {
        let mut chooser = Chooser::new(&plan(CERTAIN, 0, None));
        let mut counts = [0_u32; 4];
        for _ in 0..40_000 {
            match chooser.choose_weighted(0, &[(0, 5), (1, 1), (2, 3), (3, 0)], 0) {
                Choice::Option(label) => counts[usize::from(label)] += 1,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!((counts[0], counts[3]), (0, 0));
        assert!((9_400..10_600).contains(&counts[1]), "{counts:?}");
        assert!((29_400..30_600).contains(&counts[2]), "{counts:?}");
    }

    #[test]
    fn a_draw_picks_over_every_option_by_weight_and_ignores_the_contrary_rate() {
        let mut chooser = Chooser::new(&plan(0, 0, None));
        let defaults = (0..40_000)
            .filter(|_| chooser.draw(0, &[(0, 1), (1, 3)], 0) == Choice::Default)
            .count();
        assert!((9_400..10_600).contains(&defaults), "{defaults}");
    }

    #[test]
    fn a_held_draw_repeats_its_answer() {
        let mut chooser = Chooser::new(&ChoicePlan {
            held: vec![(4, Choice::Option(2))],
            ..plan(0, CERTAIN, None)
        });
        for _ in 0..100 {
            assert_eq!(
                chooser.draw(4, &[(0, 1), (1, 1), (2, 1)], 0),
                Choice::Option(2)
            );
        }
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
