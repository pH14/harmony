// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};

use super::{
    archive::{ArchiveKey, Slot, slot_of_key},
    campaign::CampaignTypes,
    cost::ShrinkCost,
};

pub(crate) type Lineage<G> = <<G as CampaignTypes>::Key as ArchiveKey>::Lineage;

pub(crate) struct State<G: CampaignTypes + ?Sized> {
    pub(crate) snapshot: Arc<G::Snapshot>,
    pub(crate) key: G::Key,
    pub(crate) lineage: Lineage<G>,
    pub(crate) milestones: G::Milestones,
}

#[derive(Deserialize, Serialize)]
#[serde(bound = "")]
pub(crate) struct Chain<G: CampaignTypes + ?Sized> {
    pub(crate) actions: Vec<G::Action>,
    pub(crate) costs: Vec<ShrinkCost>,
    pub(crate) snapshots: Vec<Arc<G::Snapshot>>,
    pub(crate) keys: Vec<G::Key>,
    pub(crate) lineages: Vec<Lineage<G>>,
    pub(crate) milestones: Vec<G::Milestones>,
    pub(crate) entries: Vec<Option<u64>>,
    pub(crate) slots: BTreeMap<Slot<G::Key>, usize>,
}

impl<G: CampaignTypes + ?Sized> Chain<G> {
    pub(crate) fn root(state: State<G>, entry: Option<u64>) -> Self {
        let mut chain = Self {
            actions: Vec::new(),
            costs: vec![ShrinkCost::ZERO],
            snapshots: Vec::new(),
            keys: Vec::new(),
            lineages: Vec::new(),
            milestones: Vec::new(),
            entries: Vec::new(),
            slots: BTreeMap::new(),
        };
        chain.push_state(state, entry);
        chain
    }

    pub(crate) fn truncate(&mut self, keep: usize) {
        self.actions.truncate(keep);
        self.costs.truncate(keep + 1);
        self.snapshots.truncate(keep + 1);
        self.keys.truncate(keep + 1);
        self.lineages.truncate(keep + 1);
        self.milestones.truncate(keep + 1);
        self.entries.truncate(keep + 1);
        self.slots = self
            .keys
            .iter()
            .enumerate()
            .map(|(index, key)| (slot_of_key(*key), index))
            .collect();
    }

    pub(crate) fn push(
        &mut self,
        action: G::Action,
        cost: ShrinkCost,
        state: State<G>,
        entry: Option<u64>,
    ) {
        self.actions.push(action);
        self.costs.push(cost);
        self.push_state(state, entry);
    }

    pub(crate) fn push_goal(&mut self, action: G::Action, cost: ShrinkCost) {
        self.actions.push(action);
        self.costs.push(cost);
    }

    fn push_state(&mut self, state: State<G>, entry: Option<u64>) {
        let index = self.snapshots.len();
        self.slots.insert(slot_of_key(state.key), index);
        self.snapshots.push(state.snapshot);
        self.keys.push(state.key);
        self.lineages.push(state.lineage);
        self.milestones.push(state.milestones);
        self.entries.push(entry);
    }
}
