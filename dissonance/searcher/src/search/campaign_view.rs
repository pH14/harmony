// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{CampaignCoordinator, Workload};
use crate::search::archive::{Archive, Input};
use serde::Serialize;

pub trait CampaignView<G: Workload> {
    fn archive(&self) -> &Archive<G::Action, G::Key, G::Milestones, G::Snapshot>;
    fn executions(&self) -> u64;
    fn finished(&self) -> bool;
    fn objective_witness(&self) -> Option<&Input<G::Action>>;
}

impl<G: Workload> CampaignView<G> for CampaignCoordinator<'_, G>
where
    G::ArchiveReport: Serialize,
{
    fn archive(&self) -> &Archive<G::Action, G::Key, G::Milestones, G::Snapshot> {
        CampaignCoordinator::archive(self)
    }

    fn executions(&self) -> u64 {
        CampaignCoordinator::executions(self)
    }

    fn finished(&self) -> bool {
        CampaignCoordinator::finished(self)
    }

    fn objective_witness(&self) -> Option<&Input<G::Action>> {
        CampaignCoordinator::objective_witness(self)
    }
}
