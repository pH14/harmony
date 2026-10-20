// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::search::expansion;
use std::num::NonZeroU8;

#[path = "campaign_expansion_worker.rs"]
mod worker;

pub use worker::run_expansion_task;

#[path = "campaign_expansion_coordinator.rs"]
mod coordinator;

pub use coordinator::{
    ExpansionAdmission, ExpansionCoordinator, ExpansionCounters, ExpansionDecision,
    ExpansionEdgeAdmission, ExpansionOutcome, ExpansionRunReport, ExpansionStopReason,
    replay_expansion, run_expansion_watched,
};

#[path = "campaign_expansion_stream.rs"]
mod stream;

pub use stream::{
    EXPANSION_STREAM_FORMAT, ExpansionAccountingRecord, ExpansionDuplicateRecord,
    ExpansionNodeRecord, ExpansionReservationRecord, ExpansionStreamHeader, ExpansionStreamRecord,
    ExpansionTaskRecord,
};

pub const EXPANSION_SCHEDULE: &str = "bounded_beam_fifo_tasks_v1";

pub trait ExpansionWorkload: Workload + Sized {
    type ExpansionQuality: Copy + Ord + Debug + Send + Sync;
    type ExpansionBucket: Copy + Eq + Debug + Send + Sync;

    fn expansion_policy(&self) -> &'static str;
    fn expansion_quality(&self, key: Self::Key) -> Self::ExpansionQuality;
    fn expansion_bucket(&self, key: Self::Key) -> Self::ExpansionBucket;
    fn expansion_observation_memory_charge(observation: &Self::Observations) -> usize;
    fn expansion_terminal_key(
        &self,
        target: &Self::Target,
    ) -> Result<Option<Self::Key>, Box<dyn Error>> {
        self.current_key(target).map(Some)
    }
    fn validate_expansion_action(
        &self,
        requested: Self::Action,
        recorded: Self::Action,
        disposition: crate::search::rollout::ExecutionDisposition,
    ) -> bool;
    fn merge_expansion_evidence<F>(
        &self,
        evidence: &mut Self::Evidence,
        action: &ExpansionEvidence<Self>,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<Self::Action>, Box<dyn Error>>;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExpansionEvidenceData<A, O, M, K> {
    pub requested_action: A,
    pub action: A,
    pub preparation_failure: Option<Vec<O>>,
    pub observations: Vec<O>,
    pub milestones: M,
    pub outcome: crate::search::rollout::Outcome,
    pub candidate_key: Option<K>,
    pub candidate_viable: Option<bool>,
    pub replay_certificate: Option<[u8; 32]>,
    pub result_sha256: String,
    pub execution_work: u64,
}

pub type ExpansionEvidence<G> = ExpansionEvidenceData<
    <G as CampaignTypes>::Action,
    <G as CampaignTypes>::Observations,
    <G as CampaignTypes>::Milestones,
    <G as CampaignTypes>::Key,
>;

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound = "")]
pub struct ExpansionTaskSpec<G: ExpansionWorkload> {
    pub task: u64,
    pub parent_id: u64,
    pub snapshot_id: u64,
    pub snapshot: Arc<G::Snapshot>,
    pub replay: Vec<G::Action>,
    pub replay_certificates: Vec<Option<[u8; 32]>>,
    pub parent_key: G::Key,
    pub parent_milestones: G::Milestones,
    pub root_cost_in_group: u64,
    pub last_action: Option<G::Action>,
    pub task_seed: u64,
    pub edge_budget: NonZeroU8,
}

impl<G: ExpansionWorkload> Clone for ExpansionTaskSpec<G> {
    fn clone(&self) -> Self {
        Self {
            task: self.task,
            parent_id: self.parent_id,
            snapshot_id: self.snapshot_id,
            snapshot: Arc::clone(&self.snapshot),
            replay: self.replay.clone(),
            replay_certificates: self.replay_certificates.clone(),
            parent_key: self.parent_key,
            parent_milestones: self.parent_milestones,
            root_cost_in_group: self.root_cost_in_group,
            last_action: self.last_action,
            task_seed: self.task_seed,
            edge_budget: self.edge_budget,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExpansionEdgeId {
    pub task: u64,
    pub ordinal: u32,
}

impl ExpansionEdgeId {
    pub fn observation_reservation(self) -> Result<usize, Box<dyn Error>> {
        if self.ordinal == 0 || self.ordinal >= 1 << (expansion::DEPTH + 1) {
            return Err("expansion observation has an invalid tree ordinal".into());
        }
        let value = self
            .task
            .checked_mul(1 << (expansion::DEPTH + 1))
            .and_then(|value| value.checked_add(u64::from(self.ordinal)))
            .ok_or("expansion observation identity overflow")?;
        Ok(usize::try_from(value)?)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ExpansionFacts<K, M, P, Q, B> {
    pub key: K,
    pub milestones: M,
    pub progress: P,
    pub quality: Q,
    pub bucket: B,
}

pub type WorkloadExpansionFacts<G> = ExpansionFacts<
    <G as CampaignTypes>::Key,
    <G as CampaignTypes>::Milestones,
    <<G as CampaignTypes>::Key as ArchiveKey>::Progress,
    <G as ExpansionWorkload>::ExpansionQuality,
    <G as ExpansionWorkload>::ExpansionBucket,
>;

pub struct ExpansionEdge<G: ExpansionWorkload, N> {
    pub evidence: ExpansionEvidence<G>,
    pub note: Option<N>,
}

pub type ExpansionTrace<G, N> = expansion::Trace<
    u64,
    <G as CampaignTypes>::Action,
    <G as CampaignTypes>::Snapshot,
    WorkloadExpansionFacts<G>,
    ExpansionEdge<G, N>,
>;

pub struct ExpansionTaskOutcome<G: ExpansionWorkload, N = ()> {
    pub task: u64,
    pub reconstruction_work: u64,
    pub preparation_failure: Option<Vec<G::Observations>>,
    pub trace: Option<ExpansionTrace<G, N>>,
}

pub trait ExpansionWatch<G: ExpansionWorkload>: Sync {
    type Note: Send;

    fn note_memory_charge(&self, note: &Self::Note) -> usize;

    fn start(
        &self,
        _campaign: &ExpansionCoordinator<'_, G, Self::Note>,
    ) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn admitted(
        &self,
        _admission: &mut ExpansionAdmission<G, Self::Note>,
        _campaign: &ExpansionCoordinator<'_, G, Self::Note>,
    ) {
    }

    fn run_edge(
        &self,
        id: ExpansionEdgeId,
        job: &JobSpec<G>,
        execute: impl FnOnce() -> Result<JobOutcome<G>, Box<dyn Error>>,
    ) -> WatchedJob<G, Self::Note>;
}

impl<G: ExpansionWorkload> ExpansionWatch<G> for () {
    type Note = ();

    fn note_memory_charge(&self, _note: &()) -> usize {
        0
    }

    fn run_edge(
        &self,
        _id: ExpansionEdgeId,
        _job: &JobSpec<G>,
        execute: impl FnOnce() -> Result<JobOutcome<G>, Box<dyn Error>>,
    ) -> WatchedJob<G, Self::Note> {
        execute().map(|outcome| (outcome, ()))
    }
}
