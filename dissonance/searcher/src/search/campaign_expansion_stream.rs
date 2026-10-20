// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

pub const EXPANSION_STREAM_FORMAT: &str = "dissonance-bounded-expansion-v1";

#[derive(Debug, Deserialize, Serialize)]
pub struct ExpansionStreamHeader {
    pub format: String,
    pub schedule: String,
    pub expansion_policy: String,
    pub campaign_seed: u64,
    pub window: usize,
    pub execution_budget: u64,
    pub work_budget: Option<u64>,
    pub wall_budget_seconds: Option<u64>,
    pub stop_campaign_on_objective: bool,
    pub archive_entry_limit: usize,
    pub memory_budget_mib: Option<usize>,
    pub host: String,
    pub origin: CampaignOriginRecord,
    pub workload_policies: WorkloadPolicies,
    pub workload_identity_sha256: String,
    pub executable_identity: String,
    pub action_cost_unit: String,
    pub execution_work_unit: String,
}

impl ExpansionStreamHeader {
    pub(super) fn new<G: ExpansionWorkload>(
        workload: &G,
        config: &CampaignConfig<G>,
        origin: CampaignOriginRecord,
        executable_identity: String,
        work_budget: Option<u64>,
    ) -> Self {
        Self {
            format: EXPANSION_STREAM_FORMAT.into(),
            schedule: EXPANSION_SCHEDULE.into(),
            expansion_policy: workload.expansion_policy().into(),
            campaign_seed: config.campaign_seed,
            window: config.window,
            execution_budget: config.execution_budget,
            work_budget,
            wall_budget_seconds: config.wall_budget.map(|budget| budget.as_secs()),
            stop_campaign_on_objective: config.stop_campaign_on_objective,
            archive_entry_limit: config.archive_entry_limit,
            memory_budget_mib: config.memory_budget_mib,
            host: config.host.clone(),
            origin,
            workload_policies: workload.policies(&config.run),
            workload_identity_sha256: workload.workload_identity_sha256(),
            executable_identity,
            action_cost_unit: workload.action_cost_unit().into(),
            execution_work_unit: workload.execution_work_unit().into(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound = "")]
pub struct ExpansionReservationRecord<G: ExpansionWorkload> {
    pub task: u64,
    pub parent_id: u64,
    pub snapshot_id: u64,
    pub replay: Vec<G::Action>,
    pub replay_certificates: Vec<Option<[u8; 32]>>,
    pub parent_key: G::Key,
    pub parent_milestones: G::Milestones,
    pub root_cost_in_group: u64,
    pub last_action: Option<G::Action>,
    pub task_seed: u64,
    pub edge_budget: NonZeroU8,
}

impl<G: ExpansionWorkload> From<&ExpansionTaskSpec<G>> for ExpansionReservationRecord<G> {
    fn from(spec: &ExpansionTaskSpec<G>) -> Self {
        Self {
            task: spec.task,
            parent_id: spec.parent_id,
            snapshot_id: spec.snapshot_id,
            replay: spec.replay.clone(),
            replay_certificates: spec.replay_certificates.clone(),
            parent_key: spec.parent_key,
            parent_milestones: spec.parent_milestones,
            root_cost_in_group: spec.root_cost_in_group,
            last_action: spec.last_action,
            task_seed: spec.task_seed,
            edge_budget: spec.edge_budget,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound = "")]
pub struct ExpansionNodeRecord<G: ExpansionWorkload> {
    pub ordinal: u32,
    pub parent_ordinal: Option<u32>,
    pub depth: u8,
    pub requested_action: Option<G::Action>,
    pub suffix: Vec<G::Action>,
    pub key: Option<G::Key>,
    pub milestones: Option<G::Milestones>,
    pub path_cost: u64,
    pub cost_in_group: Option<u64>,
    pub edge_cost: u64,
    pub disposition: crate::search::rollout::ExecutionDisposition,
    pub objective: bool,
    pub evidence: Option<ExpansionEvidence<G>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound = "")]
pub struct ExpansionDuplicateRecord<G: ExpansionWorkload> {
    pub ordinal: u32,
    pub existing_ordinal: u32,
    pub suffix: Vec<G::Action>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExpansionAccountingRecord {
    pub proposed_edges: usize,
    pub task_unique_edge_attempts: usize,
    pub duplicates: usize,
    pub peak_live_snapshots: usize,
}

impl From<&expansion::Accounting> for ExpansionAccountingRecord {
    fn from(value: &expansion::Accounting) -> Self {
        Self {
            proposed_edges: value.proposed_edges,
            task_unique_edge_attempts: value.task_unique_edge_attempts,
            duplicates: value.duplicates,
            peak_live_snapshots: value.peak_live_snapshots,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound = "")]
pub struct ExpansionTaskRecord<G: ExpansionWorkload> {
    pub reserved_through: u64,
    pub reservation: ExpansionReservationRecord<G>,
    pub reconstruction_work: u64,
    pub preparation_failure: Option<Vec<G::Observations>>,
    pub nodes: Vec<ExpansionNodeRecord<G>>,
    pub duplicates: Vec<ExpansionDuplicateRecord<G>>,
    pub frontier: Vec<u32>,
    pub objectives: Vec<u32>,
    pub failures: Vec<u32>,
    pub accounting: Option<ExpansionAccountingRecord>,
    pub decisions: Vec<ExpansionEdgeAdmission>,
    pub useful: bool,
}

impl<G: ExpansionWorkload> ExpansionTaskRecord<G> {
    pub fn take<N>(admission: &mut ExpansionAdmission<G, N>, reserved_through: u64) -> Self {
        let mut record = Self {
            reserved_through,
            reservation: ExpansionReservationRecord::from(&admission.spec),
            reconstruction_work: admission.outcome.reconstruction_work,
            preparation_failure: admission.outcome.preparation_failure.take(),
            nodes: Vec::new(),
            duplicates: Vec::new(),
            frontier: Vec::new(),
            objectives: Vec::new(),
            failures: Vec::new(),
            accounting: None,
            decisions: std::mem::take(&mut admission.decisions),
            useful: admission.useful,
        };
        let Some(trace) = admission.outcome.trace.as_mut() else {
            return record;
        };
        record.accounting = Some(ExpansionAccountingRecord::from(&trace.accounting));
        record.frontier = std::mem::take(&mut trace.frontier);
        record.objectives = std::mem::take(&mut trace.objectives);
        record.failures = std::mem::take(&mut trace.failures);
        record.duplicates = std::mem::take(&mut trace.duplicates)
            .into_iter()
            .map(|duplicate| ExpansionDuplicateRecord {
                ordinal: duplicate.ordinal,
                existing_ordinal: duplicate.existing_ordinal,
                suffix: duplicate.suffix,
            })
            .collect();
        for node in &mut trace.nodes {
            let evidence = node.evidence.as_mut().map(|edge| {
                let value = &mut edge.evidence;
                ExpansionEvidenceData {
                    requested_action: value.requested_action,
                    action: value.action,
                    preparation_failure: value.preparation_failure.take(),
                    observations: std::mem::take(&mut value.observations),
                    milestones: value.milestones,
                    outcome: value.outcome,
                    candidate_key: value.candidate_key,
                    candidate_viable: value.candidate_viable,
                    replay_certificate: value.replay_certificate,
                    result_sha256: std::mem::take(&mut value.result_sha256),
                    execution_work: value.execution_work,
                }
            });
            record.nodes.push(ExpansionNodeRecord {
                ordinal: node.ordinal,
                parent_ordinal: node.parent_ordinal,
                depth: node.depth,
                requested_action: node.requested_action,
                suffix: std::mem::take(&mut node.suffix),
                key: node.facts.as_ref().map(|facts| facts.key),
                milestones: node.facts.as_ref().map(|facts| facts.milestones),
                path_cost: node.path_cost,
                cost_in_group: node.cost_in_group,
                edge_cost: node.edge_cost,
                disposition: match node.status {
                    expansion::Status::Runnable => {
                        crate::search::rollout::ExecutionDisposition::Runnable
                    }
                    expansion::Status::Terminal => {
                        crate::search::rollout::ExecutionDisposition::Terminal
                    }
                    expansion::Status::Failed => {
                        crate::search::rollout::ExecutionDisposition::Failed
                    }
                },
                objective: node.objective,
                evidence,
            });
        }
        record
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "record", rename_all = "snake_case", bound = "")]
#[allow(clippy::large_enum_variant)]
pub enum ExpansionStreamRecord<G: ExpansionWorkload> {
    Task(ExpansionTaskRecord<G>),
    End {
        counters: ExpansionCounters,
        stop_reason: ExpansionStopReason,
    },
}
