// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use serde::Serialize;

use super::{
    catalog::Record,
    compile::{Accumulator, Compiled},
};

pub const THIN_TIER_STATES: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum Finding {
    UnreachableGoal {
        goal: String,
        records: usize,
    },
    ThinLeadingTier {
        stage: String,
        objective: Vec<i64>,
        states: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StageFalls {
    pub stage: String,
    pub records: usize,
}

pub struct Checker<'a> {
    strategy: &'a Compiled,
    records: usize,
    goal_met: bool,
    stage_met: Vec<bool>,
    falls: Vec<usize>,
    tiers: BTreeMap<u32, usize>,
}

impl Checker<'_> {
    pub fn observe(&mut self, record: &Record, parent: &Accumulator) {
        self.observe_action(record, parent, parent);
    }

    pub fn observe_action(
        &mut self,
        record: &Record,
        parent: &Accumulator,
        departure: &Accumulator,
    ) {
        self.records += 1;
        let mut child = parent.clone();
        let observation = self.strategy.observe(record, &mut child);
        self.goal_met |= observation.goal;
        if let (Some(from), Some(to)) = (departure.stage(), child.stage())
            && to < from
            && let Some(falls) = self.falls.get_mut(usize::from(to))
        {
            *falls += 1;
        }
        for (met, holds) in self
            .stage_met
            .iter_mut()
            .zip(self.strategy.stage_goals(record, parent))
        {
            *met |= holds;
        }
        if let Some(key) = observation.key {
            *self.tiers.entry(key.progress).or_default() += 1;
        }
    }

    #[must_use]
    pub fn findings(&self) -> Vec<Finding> {
        let stages = self.strategy.stage_names();
        let mut findings = Vec::new();
        if self.strategy.has_goal() && !self.goal_met && !self.strategy.goal_reachable() {
            findings.push(Finding::UnreachableGoal {
                goal: "goal".to_owned(),
                records: self.records,
            });
        }
        for ((name, met), reachable) in stages
            .iter()
            .zip(&self.stage_met)
            .zip(self.strategy.stages_reachable())
        {
            if !met && !reachable {
                findings.push(Finding::UnreachableGoal {
                    goal: (*name).to_owned(),
                    records: self.records,
                });
            }
        }
        if !stages.is_empty()
            && let Some((&progress, &states)) = self.tiers.last_key_value()
            && states < THIN_TIER_STATES
        {
            let view = self.strategy.view(super::key::AssertionKey {
                progress,
                ..super::key::AssertionKey::default()
            });
            findings.push(Finding::ThinLeadingTier {
                stage: view.stage.unwrap_or_default(),
                objective: view.progress,
                states,
            });
        }
        findings
    }

    #[must_use]
    pub fn stage_falls(&self) -> Vec<StageFalls> {
        self.strategy
            .stage_names()
            .into_iter()
            .zip(&self.falls)
            .map(|(stage, &records)| StageFalls {
                stage: stage.to_owned(),
                records,
            })
            .collect()
    }
}

impl Compiled {
    #[must_use]
    pub fn checker(&self) -> Checker<'_> {
        Checker {
            strategy: self,
            records: 0,
            goal_met: false,
            stage_met: vec![false; self.stage_names().len()],
            falls: vec![0; self.stage_names().len()],
            tiers: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn check<'a>(
        &self,
        observed: impl IntoIterator<Item = (&'a Record, &'a Accumulator)>,
    ) -> Vec<Finding> {
        let mut checker = self.checker();
        for (record, parent) in observed {
            checker.observe(record, parent);
        }
        checker.findings()
    }
}
