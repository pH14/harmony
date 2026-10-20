// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

pub const EXPANSION_STREAM_FORMAT: &str = "dissonance-bounded-expansion-v2";

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

#[derive(Debug)]
pub struct ExpansionObservations<O>(ObservationStorage<O>);

#[derive(Debug)]
enum ObservationStorage<O> {
    Raw(Vec<O>),
    Digest { count: u64, sha256: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservationDigest {
    count: u64,
    sha256: String,
}

impl<O> ExpansionObservations<O> {
    fn raw(observations: Vec<O>) -> Self {
        Self(ObservationStorage::Raw(observations))
    }

    pub fn count(&self) -> u64 {
        match &self.0 {
            ObservationStorage::Raw(values) => values.len() as u64,
            ObservationStorage::Digest { count, .. } => *count,
        }
    }
}

impl<O: Serialize> Serialize for ExpansionObservations<O> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let fresh;
        let (count, sha256) = match &self.0 {
            ObservationStorage::Raw(values) => {
                fresh = postcard_value_sha256(values).map_err(serde::ser::Error::custom)?;
                (values.len() as u64, fresh.as_str())
            }
            ObservationStorage::Digest { count, sha256 } => (*count, sha256.as_str()),
        };
        let mut output = serializer.serialize_struct("ObservationDigest", 2)?;
        output.serialize_field("count", &count)?;
        output.serialize_field("sha256", sha256)?;
        output.end()
    }
}

impl<'de, O> Deserialize<'de> for ExpansionObservations<O> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let digest = ObservationDigest::deserialize(deserializer)?;
        if digest.sha256.len() != 64
            || !digest
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(serde::de::Error::custom(
                "invalid expansion observation SHA-256",
            ));
        }
        Ok(Self(ObservationStorage::Digest {
            count: digest.count,
            sha256: digest.sha256,
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound = "")]
pub struct ExpansionEvidenceRecord<G: ExpansionWorkload> {
    pub requested_action: G::Action,
    pub action: G::Action,
    pub preparation_failure: Option<ExpansionObservations<G::Observations>>,
    pub observations: ExpansionObservations<G::Observations>,
    pub milestones: G::Milestones,
    pub outcome: crate::search::rollout::Outcome,
    pub candidate_key: Option<G::Key>,
    pub candidate_viable: Option<bool>,
    pub replay_certificate: Option<[u8; 32]>,
    pub result_sha256: String,
    pub execution_work: u64,
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
    pub evidence: Option<ExpansionEvidenceRecord<G>>,
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
    pub preparation_failure: Option<ExpansionObservations<G::Observations>>,
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
            preparation_failure: admission
                .outcome
                .preparation_failure
                .take()
                .map(ExpansionObservations::raw),
            nodes: Vec::with_capacity(
                admission
                    .outcome
                    .trace
                    .as_ref()
                    .map_or(0, |trace| trace.nodes.len()),
            ),
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
                ExpansionEvidenceRecord::<G> {
                    requested_action: value.requested_action,
                    action: value.action,
                    preparation_failure: value
                        .preparation_failure
                        .take()
                        .map(ExpansionObservations::raw),
                    observations: ExpansionObservations::raw(std::mem::take(
                        &mut value.observations,
                    )),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_observation_commitments_roundtrip_exact_values_and_option_presence() {
        for values in [
            Vec::<String>::new(),
            vec!["level λ→終".into(), "\u{0}é".into()],
        ] {
            let expected = postcard_value_sha256(&values).unwrap();
            let raw = ExpansionObservations::raw(values.clone());
            let bytes = serde_json::to_vec(&raw).unwrap();
            let decoded: ExpansionObservations<String> = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(decoded.count(), values.len() as u64);
            assert_eq!(serde_json::to_vec(&decoded).unwrap(), bytes);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["sha256"],
                expected
            );
        }
        let one = ExpansionObservations::raw(vec![vec![1_u8, 2], vec![3]]);
        let changed = ExpansionObservations::raw(vec![vec![1_u8, 9], vec![3]]);
        assert_eq!(one.count(), changed.count());
        assert_ne!(
            serde_json::to_vec(&one).unwrap(),
            serde_json::to_vec(&changed).unwrap()
        );
        let absent: Option<ExpansionObservations<u8>> = None;
        let empty = Some(ExpansionObservations::raw(Vec::<u8>::new()));
        assert_ne!(
            serde_json::to_vec(&absent).unwrap(),
            serde_json::to_vec(&empty).unwrap()
        );
    }

    #[test]
    fn expansion_observation_commitments_reject_malformed_wire_without_count_allocation() {
        let valid = serde_json::json!({"count": 1, "sha256": "0".repeat(64)});
        for field in [
            "missing",
            "short",
            "uppercase",
            "nonhex",
            "negative",
            "overflow",
            "boolean",
            "extra",
            "array",
        ] {
            let mut value = valid.clone();
            match field {
                "missing" => {
                    value.as_object_mut().unwrap().remove("sha256");
                }
                "short" => value["sha256"] = "0".repeat(63).into(),
                "uppercase" => value["sha256"] = "A".repeat(64).into(),
                "nonhex" => value["sha256"] = "g".repeat(64).into(),
                "negative" => value["count"] = (-1).into(),
                "overflow" => value["count"] = "18446744073709551616".into(),
                "boolean" => value["count"] = true.into(),
                "extra" => value["observations"] = serde_json::json!([1]),
                "array" => value = serde_json::json!([1]),
                _ => unreachable!(),
            }
            assert!(
                serde_json::from_value::<ExpansionObservations<u8>>(value).is_err(),
                "accepted {field}"
            );
        }
        let bytes = serde_json::to_vec(&valid).unwrap();
        assert!(
            serde_json::from_slice::<ExpansionObservations<u8>>(&bytes[..bytes.len() - 1]).is_err()
        );
        let huge: ExpansionObservations<u8> = serde_json::from_value(serde_json::json!({
            "count": u64::MAX, "sha256": "0".repeat(64)
        }))
        .unwrap();
        assert_eq!(huge.count(), u64::MAX);
        assert!(serde_json::to_vec(&huge).unwrap().len() < 128);
        assert!(
            serde_json::from_str::<ExpansionObservations<u8>>(&format!(
                "{{\"count\":18446744073709551616,\"sha256\":\"{}\"}}",
                "0".repeat(64)
            ))
            .is_err()
        );
    }

    #[test]
    fn expansion_observation_serialization_failure_preserves_only_the_earlier_stream_prefix() {
        #[derive(Debug)]
        struct Observation(bool);
        impl Serialize for Observation {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                if self.0 {
                    return Err(serde::ser::Error::custom(
                        "injected observation encoding failure",
                    ));
                }
                serializer.serialize_u8(7)
            }
        }
        let first = || ExpansionObservations::raw(vec![Observation(false)]);
        let mut expected = b"0\n".to_vec();
        serde_json::to_writer(&mut expected, &first()).unwrap();
        expected.push(b'\n');
        for background in [false, true] {
            let mut sink = Vec::new();
            let mut writer = StreamWriter::<ExpansionObservations<Observation>>::new(&mut sink);
            writer.write_header(&0).unwrap();
            if background {
                writer.enable_background(None).unwrap();
            }
            writer.write_line(first()).unwrap();
            let failed = writer.write_line(ExpansionObservations::raw(vec![Observation(true)]));
            if !background {
                assert!(failed.is_err());
            }
            let error = writer.finish().unwrap_err();
            assert!(!error.to_string().is_empty());
            assert_eq!(sink, expected);
        }
    }
}
