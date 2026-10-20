// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::search::archive::{ArchiveKey, RetentionPolicy};
use crate::search::rand::RomuDuoJrRand;
use crate::search::rollout::{ExecutionDisposition, Outcome};
use std::mem::size_of;

const TASK_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const TASK_PATH_BYTES: usize = 1024 * 1024;

fn checked_charge(parts: impl IntoIterator<Item = usize>) -> Result<usize, Box<dyn Error>> {
    let mut sum = 0_usize;
    for part in parts {
        sum = sum
            .checked_add(part)
            .ok_or("expansion memory charge overflow")?;
    }
    Ok(sum)
}

fn owned_vector_bytes<T>(
    values: &[T],
    capacity: usize,
    charge: impl Fn(&T) -> usize,
) -> Result<usize, Box<dyn Error>> {
    if capacity < values.len() {
        return Err("expansion observation capacity is below its length".into());
    }
    let inline = size_of::<T>();
    let allocation = capacity
        .checked_mul(inline)
        .ok_or("expansion observation memory charge overflow")?;
    let mut total = crate::search::heap::block(allocation);
    for value in values {
        let heap = charge(value)
            .checked_sub(inline)
            .ok_or("expansion observation charge omits its inline bytes")?;
        total = total
            .checked_add(heap)
            .ok_or("expansion observation memory charge overflow")?;
    }
    Ok(total)
}

fn worker_facts<G: ExpansionWorkload>(
    workload: &G,
    key: G::Key,
    milestones: G::Milestones,
) -> WorkloadExpansionFacts<G> {
    ExpansionFacts {
        key,
        milestones,
        progress: key.progress(),
        quality: workload.expansion_quality(key),
        bucket: workload.expansion_bucket(key),
    }
}

struct WorkerAdapter<'a, G: ExpansionWorkload, W: ExpansionWatch<G>> {
    workload: &'a G,
    run: &'a G::Run,
    target: &'a mut G::Target,
    task: u64,
    watch: &'a W,
}

impl<G: ExpansionWorkload, W: ExpansionWatch<G>> expansion::Adapter for WorkerAdapter<'_, G, W> {
    type Error = Box<dyn Error>;
    type RootId = u64;
    type Action = G::Action;
    type Snapshot = G::Snapshot;
    type Facts = WorkloadExpansionFacts<G>;
    type Progress = <G::Key as ArchiveKey>::Progress;
    type Quality = G::ExpansionQuality;
    type Bucket = G::ExpansionBucket;
    type Evidence = ExpansionEdge<G, W::Note>;

    fn progress(facts: &Self::Facts) -> &Self::Progress {
        &facts.progress
    }

    fn quality(facts: &Self::Facts) -> &Self::Quality {
        &facts.quality
    }

    fn bucket(facts: &Self::Facts) -> &Self::Bucket {
        &facts.bucket
    }

    fn actions(
        &mut self,
        parent: expansion::ParentOf<'_, Self>,
        entropy: [u64; expansion::FANOUT],
    ) -> Result<[Option<Self::Action>; expansion::FANOUT], Box<dyn Error>> {
        let previous = parent.suffix.last().or(parent.root.last_action.as_ref());
        let mut actions = [None; expansion::FANOUT];
        for (slot, seed) in actions.iter_mut().zip(entropy) {
            *slot = Some(self.workload.sample_alphabet(
                self.run,
                previous,
                &mut RomuDuoJrRand::with_seed(seed),
            )?);
        }
        Ok(actions)
    }

    fn execute(
        &mut self,
        requests: &[expansion::RequestOf<Self>],
    ) -> Result<Vec<expansion::CompletionOf<Self>>, Box<dyn Error>> {
        let mut completions = Vec::with_capacity(requests.len());
        for request in requests {
            let id = ExpansionEdgeId {
                task: self.task,
                ordinal: request.ordinal,
            };
            let reservation = id.observation_reservation()?;
            let job = JobSpec::<G> {
                reservation,
                planned_finish: 0,
                snapshot: Arc::clone(&request.parent_snapshot),
                replay: Vec::new(),
                replay_certificates: Vec::new(),
                parent_milestones: request.parent_facts.milestones,
                suffix: vec![request.action],
            };
            let workload = self.workload;
            let run = self.run;
            let target = &mut *self.target;
            let (outcome, note) = self.watch.run_edge(id, &job, || {
                let before = workload.execution_work(target);
                let result = workload.execute_job_certified(
                    run,
                    target,
                    &job.snapshot,
                    &[],
                    &[],
                    job.parent_milestones,
                    &job.suffix,
                    RetentionPolicy::Unprobed,
                    true,
                )?;
                workload.verify_integrity()?;
                let result_sha256 = workload.certified_result_sha256(&result, target)?;
                let execution_work = execution_work_delta(
                    before,
                    workload.execution_work(target),
                    "expansion edge",
                )?;
                Ok(JobOutcome {
                    reservation,
                    result,
                    execution_work,
                    result_sha256,
                })
            })?;
            if outcome.reservation != reservation {
                return Err("expansion edge returned an incorrect observation identity".into());
            }
            let preparation_failure = outcome.result.preparation_failure;
            let mut actions = outcome.result.actions;
            let action = if preparation_failure.is_some() {
                if !actions.is_empty() {
                    return Err("expansion preparation failure also returned actions".into());
                }
                CampaignActionResult::<G> {
                    action: request.action,
                    observations: Vec::new(),
                    milestones: job.parent_milestones,
                    outcome: Outcome {
                        objective_reached: false,
                        disposition: ExecutionDisposition::Failed,
                    },
                    candidate: None,
                }
            } else {
                if actions.len() != 1 {
                    return Err("expansion edge must return exactly one action".into());
                }
                actions.pop().ok_or("expansion action result missing")?
            };
            let disposition = action.outcome.disposition;
            if !self
                .workload
                .validate_expansion_action(request.action, action.action, disposition)
            {
                return Err("expansion returned an invalid recorded action".into());
            }
            let replay_certificate = match &action.candidate {
                Some(candidate) => self
                    .workload
                    .candidate_replay_certificate(&candidate.snapshot)?,
                None => None,
            };
            let candidate_key = action.candidate.as_ref().map(|candidate| candidate.key);
            let candidate_viable = action.candidate.as_ref().map(|candidate| candidate.viable);
            let (facts, snapshot, status) = match disposition {
                ExecutionDisposition::Runnable => {
                    let candidate = action
                        .candidate
                        .ok_or("runnable expansion endpoint lacks a candidate")?;
                    if !candidate.viable {
                        return Err("runnable expansion endpoint has a nonviable candidate".into());
                    }
                    let key = self
                        .workload
                        .complete_candidate_key(candidate.key, &candidate.snapshot)?;
                    (
                        Some(worker_facts(self.workload, key, action.milestones)),
                        Some(Arc::new(candidate.snapshot)),
                        expansion::Status::Runnable,
                    )
                }
                ExecutionDisposition::Terminal => (
                    self.workload
                        .expansion_terminal_key(self.target)?
                        .map(|key| worker_facts(self.workload, key, action.milestones)),
                    None,
                    expansion::Status::Terminal,
                ),
                ExecutionDisposition::Failed => (None, None, expansion::Status::Failed),
            };
            completions.push(expansion::Completion {
                ordinal: request.ordinal,
                recorded_action: action.action,
                status,
                objective: action.outcome.objective_reached,
                facts,
                edge_cost: if preparation_failure.is_some() {
                    0
                } else {
                    (self.workload.action_cost_fn())(&action.action)
                },
                evidence: ExpansionEdge {
                    evidence: ExpansionEvidenceData {
                        requested_action: request.action,
                        action: action.action,
                        preparation_failure,
                        observations: action.observations,
                        milestones: action.milestones,
                        outcome: action.outcome,
                        candidate_key,
                        candidate_viable,
                        replay_certificate,
                        result_sha256: outcome.result_sha256,
                        execution_work: outcome.execution_work,
                    },
                    note: Some(note),
                },
                snapshot,
            });
        }
        Ok(completions)
    }

    fn validate_recorded_action(
        &self,
        requested: &Self::Action,
        recorded: &Self::Action,
        status: expansion::Status,
    ) -> bool {
        let disposition = match status {
            expansion::Status::Runnable => ExecutionDisposition::Runnable,
            expansion::Status::Terminal => ExecutionDisposition::Terminal,
            expansion::Status::Failed => ExecutionDisposition::Failed,
        };
        self.workload
            .validate_expansion_action(*requested, *recorded, disposition)
    }

    fn snapshot_bytes(&self, snapshot: &Self::Snapshot) -> Result<usize, Box<dyn Error>> {
        checked_charge([
            G::snapshot_memory_charge(snapshot),
            size_of::<G::Snapshot>(),
            2 * size_of::<usize>(),
        ])
    }

    fn evidence_bytes(&self, edge: &Self::Evidence) -> Result<usize, Box<dyn Error>> {
        let evidence = &edge.evidence;
        let observations = owned_vector_bytes(
            &evidence.observations,
            evidence.observations.capacity(),
            G::expansion_observation_memory_charge,
        )?;
        let preparation = evidence
            .preparation_failure
            .as_ref()
            .map(|values| {
                owned_vector_bytes(
                    values,
                    values.capacity(),
                    G::expansion_observation_memory_charge,
                )
            })
            .transpose()?
            .unwrap_or(0);
        let note_heap = edge
            .note
            .as_ref()
            .map(|note| {
                self.watch
                    .note_memory_charge(note)
                    .checked_sub(size_of::<W::Note>())
                    .ok_or_else(|| -> Box<dyn Error> {
                        "expansion watch note charge omits its inline bytes".into()
                    })
            })
            .transpose()?
            .unwrap_or(0);
        checked_charge([
            size_of::<Self::Evidence>(),
            crate::search::heap::block(evidence.result_sha256.capacity()),
            observations,
            preparation,
            note_heap,
        ])
    }

    fn facts_bytes(&self, _facts: &Self::Facts) -> Result<usize, Box<dyn Error>> {
        checked_charge([size_of::<Self::Facts>(), 2 * size_of::<usize>()])
    }

    fn action_heap_bytes(&self, _action: &Self::Action) -> Result<usize, Box<dyn Error>> {
        Ok(0)
    }
}

pub fn run_expansion_task<G: ExpansionWorkload, W: ExpansionWatch<G>>(
    workload: &G,
    run: &G::Run,
    target: &mut G::Target,
    spec: &ExpansionTaskSpec<G>,
    watch: &W,
) -> Result<ExpansionTaskOutcome<G, W::Note>, Box<dyn Error>> {
    if spec.replay.len() != spec.replay_certificates.len() {
        return Err("expansion replay certificate count differs from replay length".into());
    }
    let (snapshot, reconstruction_work) = if spec.replay.is_empty() {
        (Arc::clone(&spec.snapshot), 0)
    } else {
        let before = workload.execution_work(target);
        let preparation = workload.execute_job_certified(
            run,
            target,
            &spec.snapshot,
            &spec.replay,
            &spec.replay_certificates,
            spec.parent_milestones,
            &[],
            RetentionPolicy::Unprobed,
            true,
        )?;
        workload.verify_integrity()?;
        let reconstruction_work = execution_work_delta(
            before,
            workload.execution_work(target),
            "expansion reconstruction",
        )?;
        if !preparation.actions.is_empty() {
            return Err("expansion root reconstruction unexpectedly executed a suffix".into());
        }
        if let Some(observations) = preparation.preparation_failure {
            return Ok(ExpansionTaskOutcome {
                task: spec.task,
                reconstruction_work,
                preparation_failure: Some(observations),
                trace: None,
            });
        }
        if workload.current_key(target)? != spec.parent_key
            || workload.execution_disposition(target) != ExecutionDisposition::Runnable
        {
            return Err(
                "expansion reconstruction did not reach its runnable selected parent".into(),
            );
        }
        (Arc::new(workload.snapshot(target)?), reconstruction_work)
    };
    let root = expansion::Root {
        parent_id: spec.parent_id,
        facts: Arc::new(worker_facts(
            workload,
            spec.parent_key,
            spec.parent_milestones,
        )),
        cost_in_group: spec.root_cost_in_group,
        last_action: spec.last_action,
        task_seed: spec.task_seed,
        edge_budget: spec.edge_budget,
    };
    let mut adapter = WorkerAdapter {
        workload,
        run,
        target,
        task: spec.task,
        watch,
    };
    let trace = expansion::run(
        &mut adapter,
        root,
        snapshot,
        expansion::Budget {
            snapshot_bytes: TASK_PAYLOAD_BYTES,
            evidence_bytes: TASK_PAYLOAD_BYTES,
            path_bytes: TASK_PATH_BYTES,
        },
    )
    .map_err(|error| match error {
        expansion::Error::Adapter(error) => error,
        error => Box::new(error) as Box<dyn Error>,
    })?;
    Ok(ExpansionTaskOutcome {
        task: spec.task,
        reconstruction_work,
        preparation_failure: None,
        trace: Some(trace),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_vector_charge_counts_spare_inline_storage_and_owned_heap_once() {
        let mut observations = Vec::with_capacity(8);
        observations.push(vec![1_u8, 2, 3]);
        observations.push(vec![4_u8; 5]);
        let charge =
            |value: &Vec<u8>| size_of::<Vec<u8>>() + crate::search::heap::block(value.capacity());
        let expected = crate::search::heap::block(observations.capacity() * size_of::<Vec<u8>>())
            + observations
                .iter()
                .map(|value| crate::search::heap::block(value.capacity()))
                .sum::<usize>();
        assert_eq!(
            owned_vector_bytes(&observations, observations.capacity(), charge).unwrap(),
            expected,
        );
    }

    #[test]
    fn evidence_vector_charge_rejects_undercharged_inline_and_overflow() {
        assert!(owned_vector_bytes(&[7_u64], 1, |_| 7).is_err());
        assert!(owned_vector_bytes(&[7_u64], 0, |_| 8).is_err());
        assert!(owned_vector_bytes(&[7_u64], usize::MAX, |_| 8).is_err());
        assert!(owned_vector_bytes(&[7_u64, 8], 2, |_| usize::MAX).is_err());
        assert!(checked_charge([usize::MAX, 1]).is_err());
        assert_eq!(owned_vector_bytes::<()>(&[], usize::MAX, |_| 0).unwrap(), 0);
    }
}

#[cfg(test)]
mod execution_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
    struct Key(u8);

    impl ArchiveKey for Key {
        type Place = u8;
        type Progress = u8;
        type Identity = u8;
        type Lineage = ();
        fn place(self) -> u8 {
            self.0 % 2
        }
        fn progress(self) -> u8 {
            0
        }
        fn identity(self) -> u8 {
            self.0
        }
        fn complete(self, _: Option<(Self, &())>) -> Self {
            self
        }
        fn record(_: &mut (), _: Self) {}
    }

    #[derive(Clone, Copy)]
    enum Mode {
        Normal,
        RootPreparation,
        EdgePreparation,
        Terminal,
        Integrity,
        TerminalKeyError,
    }

    struct TestWorkload {
        mode: Mode,
        restores: AtomicUsize,
        replays: AtomicUsize,
        snapshots: AtomicUsize,
    }

    impl TestWorkload {
        fn new(mode: Mode) -> Self {
            Self {
                mode,
                restores: AtomicUsize::new(0),
                replays: AtomicUsize::new(0),
                snapshots: AtomicUsize::new(0),
            }
        }
    }

    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct Report {
        entries: Vec<ArchiveEntryReport<u8, Key, u8>>,
    }

    #[derive(Default)]
    struct Target {
        value: u8,
        work: u64,
    }

    #[derive(Debug)]
    struct IntegrityFailure;
    impl std::fmt::Display for IntegrityFailure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("typed worker integrity failure")
        }
    }
    impl Error for IntegrityFailure {}

    impl CampaignTypes for TestWorkload {
        type JobObservation = ();
        type Target = Target;
        type Action = u8;
        type Key = Key;
        type Milestones = u8;
        type Progress = u8;
        type Snapshot = u8;
        type Observations = u8;
        type Evidence = ();
        type ArchiveReport = Report;
        type Run = ();
    }

    impl Reporting for TestWorkload {
        fn write_evidence_checkpoint(
            evidence: &(),
            out: &mut dyn Write,
        ) -> Result<(), Box<dyn Error>> {
            out.write_all(&postcard::to_allocvec(evidence)?)?;
            Ok(())
        }
        fn evidence_from_checkpoint(bytes: &[u8]) -> Result<(), Box<dyn Error>> {
            Ok(postcard::from_bytes(bytes)?)
        }
        fn stream_format(&self) -> &'static str {
            "expansion-worker-test"
        }
        fn checkpoint_format(&self) -> &'static str {
            "expansion-worker-test"
        }
        fn workload_identity_sha256(&self) -> String {
            "worker-fixture".into()
        }
        fn action_cost_unit(&self) -> &'static str {
            "steps"
        }
        fn execution_work_unit(&self) -> &'static str {
            "steps"
        }
        fn result_sha256(
            &self,
            result: &CampaignJobResult<Self>,
        ) -> Result<String, Box<dyn Error>> {
            postcard_result_sha256::<Self>(result)
        }
        fn snapshot_identity_sha256(&self, snapshot: &u8) -> Result<String, Box<dyn Error>> {
            Ok(snapshot.to_string())
        }
        fn archive_report(&self, _: &(), state: ArchiveReportState<Self>) -> Report {
            Report {
                entries: state.entries,
            }
        }
    }

    impl InputPolicy for TestWorkload {
        fn policies(&self, _: &()) -> WorkloadPolicies {
            WorkloadPolicies::new()
        }
        fn resolve_recorded(&self, _: &WorkloadPolicies) -> Result<(), Box<dyn Error>> {
            Ok(())
        }
        fn sample_alphabet(
            &self,
            _: &(),
            _: Option<&u8>,
            random: &mut RomuDuoJrRand,
        ) -> Result<u8, Box<dyn Error>> {
            Ok(if matches!(self.mode, Mode::Normal) {
                1 + (random.next_u64() % 8) as u8
            } else {
                4
            })
        }
    }

    impl TargetExecution for TestWorkload {
        fn new_target(&self) -> Result<Target, String> {
            Ok(Target::default())
        }
        fn reset(&self, target: &mut Target) {
            target.value = 0;
        }
        fn restore(&self, target: &mut Target, snapshot: &u8) -> Result<(), Box<dyn Error>> {
            self.restores.fetch_add(1, Ordering::Relaxed);
            target.value = *snapshot;
            Ok(())
        }
        fn execution_work(&self, target: &Target) -> u64 {
            target.work
        }
        fn action_cost_fn(&self) -> fn(&u8) -> u64 {
            |action| u64::from(*action)
        }
        fn snapshot_memory_charge(_: &u8) -> usize {
            256 << 10
        }
        fn apply_action(
            &self,
            target: &mut Target,
            action: &u8,
            milestones: &mut u8,
        ) -> Result<(), Box<dyn Error>> {
            target.value += *action;
            target.work += u64::from(*action);
            *milestones += *action;
            Ok(())
        }
        fn snapshot(&self, target: &mut Target) -> Result<u8, Box<dyn Error>> {
            self.snapshots.fetch_add(1, Ordering::Relaxed);
            Ok(target.value)
        }
        fn candidate_replay_certificate(
            &self,
            snapshot: &u8,
        ) -> Result<Option<[u8; 32]>, Box<dyn Error>> {
            Ok(Some([*snapshot; 32]))
        }
        fn verify_integrity(&self) -> Result<(), Box<dyn Error>> {
            if matches!(self.mode, Mode::Integrity) {
                Err(Box::new(IntegrityFailure))
            } else {
                Ok(())
            }
        }
        #[allow(clippy::too_many_arguments)]
        fn execute_job_certified(
            &self,
            _: &(),
            target: &mut Target,
            snapshot: &u8,
            replay: &[u8],
            certificates: &[Option<[u8; 32]>],
            milestones: u8,
            suffix: &[u8],
            _: RetentionPolicy,
            _: bool,
        ) -> Result<CampaignJobResult<Self>, Box<dyn Error>> {
            assert_eq!(replay.len(), certificates.len());
            self.restore(target, snapshot)?;
            if !replay.is_empty() {
                self.replays.fetch_add(1, Ordering::Relaxed);
            }
            let mut milestones = milestones;
            for action in replay {
                self.apply_action(target, action, &mut milestones)?;
            }
            let preparation = matches!(self.mode, Mode::RootPreparation) && !replay.is_empty()
                || matches!(self.mode, Mode::EdgePreparation) && !suffix.is_empty();
            if preparation {
                return Ok(CampaignJobResult::<Self> {
                    preparation_failure: Some(vec![17]),
                    actions: Vec::new(),
                });
            }
            let mut actions = Vec::new();
            for requested in suffix {
                let recorded = if matches!(self.mode, Mode::Terminal | Mode::TerminalKeyError) {
                    requested / 2
                } else {
                    *requested
                };
                self.apply_action(target, &recorded, &mut milestones)?;
                let terminal = matches!(self.mode, Mode::Terminal | Mode::TerminalKeyError);
                actions.push(CampaignActionResult::<Self> {
                    action: recorded,
                    observations: vec![target.value],
                    milestones,
                    outcome: Outcome {
                        objective_reached: false,
                        disposition: if terminal {
                            ExecutionDisposition::Terminal
                        } else {
                            ExecutionDisposition::Runnable
                        },
                    },
                    candidate: if terminal {
                        None
                    } else {
                        Some(CampaignCandidate::<Self> {
                            key: Key(target.value),
                            viable: true,
                            snapshot: self.snapshot(target)?,
                        })
                    },
                });
            }
            Ok(CampaignJobResult::<Self> {
                preparation_failure: None,
                actions,
            })
        }
    }

    impl Evaluation for TestWorkload {
        fn execution_disposition(&self, _: &Target) -> ExecutionDisposition {
            ExecutionDisposition::Runnable
        }
        fn objective_reached(&self, _: &(), _: &Target) -> Result<bool, Box<dyn Error>> {
            Ok(false)
        }
        fn current_key(&self, target: &Target) -> Result<Key, Box<dyn Error>> {
            Ok(Key(target.value))
        }
        fn complete_candidate_key(&self, key: Key, snapshot: &u8) -> Result<Key, Box<dyn Error>> {
            assert_eq!(key.0, *snapshot);
            Ok(key)
        }
        fn merge_milestones(&self, _: &mut u8, _: u8) {}
        fn aggregate_milestones(_: &()) -> u8 {
            0
        }
        fn aggregate_progress(_: &()) -> u8 {
            0
        }
        fn merge_origin_evidence(&self, _: &mut (), _: &Report) {}
        fn merge_snapshot_root_evidence(
            &self,
            _: &mut (),
            _: &Target,
        ) -> Result<(), Box<dyn Error>> {
            Ok(())
        }
        fn merge_import_evidence(&self, _: &mut (), _: u8, _: &Input<u8>) {}
        fn merge_action_evidence<F>(
            &self,
            _: &mut (),
            _: &CampaignActionResult<Self>,
            _: u64,
            _: F,
        ) -> Result<(), Box<dyn Error>>
        where
            F: FnOnce() -> Result<Input<u8>, Box<dyn Error>>,
        {
            Ok(())
        }
        fn source_entries<'a>(&self, source: &'a Report) -> &'a [ArchiveEntryReport<u8, Key, u8>] {
            &source.entries
        }
        fn resume_input(&self, _: &Report) -> Result<Input<u8>, Box<dyn Error>> {
            Ok(Input::default())
        }
    }

    impl ExpansionWorkload for TestWorkload {
        type ExpansionQuality = std::cmp::Reverse<u8>;
        type ExpansionBucket = u8;
        fn expansion_policy(&self) -> &'static str {
            "worker-test"
        }
        fn expansion_quality(&self, key: Key) -> Self::ExpansionQuality {
            std::cmp::Reverse(key.0)
        }
        fn expansion_bucket(&self, key: Key) -> u8 {
            key.place()
        }
        fn expansion_observation_memory_charge(_: &u8) -> usize {
            1
        }
        fn expansion_terminal_key(&self, _: &Target) -> Result<Option<Key>, Box<dyn Error>> {
            if matches!(self.mode, Mode::TerminalKeyError) {
                Err(Box::new(IntegrityFailure))
            } else {
                Ok(None)
            }
        }
        fn validate_expansion_action(
            &self,
            requested: u8,
            recorded: u8,
            disposition: ExecutionDisposition,
        ) -> bool {
            if disposition == ExecutionDisposition::Runnable {
                requested == recorded
            } else {
                recorded > 0 && recorded <= requested
            }
        }
        fn merge_expansion_evidence<F>(
            &self,
            _: &mut (),
            _: &ExpansionEvidence<Self>,
            _: u64,
            _: F,
        ) -> Result<(), Box<dyn Error>>
        where
            F: FnOnce() -> Result<Input<u8>, Box<dyn Error>>,
        {
            Ok(())
        }
    }

    fn spec(replay: Vec<u8>) -> ExpansionTaskSpec<TestWorkload> {
        let value = replay.iter().copied().sum();
        ExpansionTaskSpec {
            task: 3,
            parent_id: 9,
            snapshot_id: 8,
            snapshot: Arc::new(0),
            replay_certificates: vec![None; replay.len()],
            replay,
            parent_key: Key(value),
            parent_milestones: value,
            root_cost_in_group: 7,
            last_action: Some(2),
            task_seed: 19,
            edge_budget: NonZeroU8::new(14).unwrap(),
        }
    }

    struct Watch {
        root: Arc<u8>,
        root_edges: AtomicUsize,
    }
    impl ExpansionWatch<TestWorkload> for Watch {
        type Note = usize;
        fn note_memory_charge(&self, _: &usize) -> usize {
            size_of::<usize>()
        }
        fn run_edge(
            &self,
            id: ExpansionEdgeId,
            job: &JobSpec<TestWorkload>,
            execute: impl FnOnce() -> Result<JobOutcome<TestWorkload>, Box<dyn Error>>,
        ) -> WatchedJob<TestWorkload, usize> {
            assert!(job.replay.is_empty());
            assert!(job.replay_certificates.is_empty());
            assert_eq!(job.suffix.len(), 1);
            if id.ordinal <= 2 {
                assert!(Arc::ptr_eq(&self.root, &job.snapshot));
                self.root_edges.fetch_add(1, Ordering::Relaxed);
            }
            assert_eq!(job.reservation, id.observation_reservation().unwrap());
            execute().map(|outcome| (outcome, job.reservation))
        }
    }

    #[test]
    fn native_worker_reuses_empty_root_arc_and_records_exact_edge_facts_work_and_notes() {
        let workload = TestWorkload::new(Mode::Normal);
        let spec = spec(Vec::new());
        let watch = Watch {
            root: Arc::clone(&spec.snapshot),
            root_edges: AtomicUsize::new(0),
        };
        let outcome =
            run_expansion_task(&workload, &(), &mut Target::default(), &spec, &watch).unwrap();
        assert_eq!(outcome.reconstruction_work, 0);
        let trace = outcome.trace.unwrap();
        assert!(trace.nodes.len() > 2);
        assert!(watch.root_edges.load(Ordering::Relaxed) > 0);
        assert_eq!(
            workload.restores.load(Ordering::Relaxed),
            trace.nodes.len() - 1
        );
        assert_eq!(
            workload.snapshots.load(Ordering::Relaxed),
            trace.nodes.len() - 1
        );
        for node in &trace.nodes[1..] {
            let edge = node.evidence.as_ref().unwrap();
            let facts = node.facts.as_ref().unwrap();
            assert_eq!(facts.key.0, node.suffix.iter().copied().sum::<u8>());
            assert_eq!(node.edge_cost, u64::from(edge.evidence.action));
            assert_eq!(edge.evidence.execution_work, node.edge_cost);
            assert_eq!(
                edge.note,
                Some(
                    ExpansionEdgeId {
                        task: spec.task,
                        ordinal: node.ordinal
                    }
                    .observation_reservation()
                    .unwrap()
                )
            );
            assert_eq!(edge.evidence.replay_certificate, Some([facts.key.0; 32]));
            assert!(!edge.evidence.result_sha256.is_empty());
        }
    }

    #[test]
    fn native_worker_reconstructs_selected_parent_once_and_preserves_root_preparation_failure() {
        let spec = spec(vec![1, 2]);
        let workload = TestWorkload::new(Mode::Normal);
        let result =
            run_expansion_task(&workload, &(), &mut Target::default(), &spec, &()).unwrap();
        let trace = result.trace.unwrap();
        assert_eq!(result.reconstruction_work, 3);
        assert_eq!(workload.replays.load(Ordering::Relaxed), 1);
        assert_eq!(workload.restores.load(Ordering::Relaxed), trace.nodes.len());
        assert_eq!(
            workload.snapshots.load(Ordering::Relaxed),
            trace.nodes.len()
        );
        let workload = TestWorkload::new(Mode::RootPreparation);
        let failed =
            run_expansion_task(&workload, &(), &mut Target::default(), &spec, &()).unwrap();
        assert_eq!(failed.preparation_failure, Some(vec![17]));
        assert!(failed.trace.is_none());
        assert_eq!(failed.reconstruction_work, 3);
        assert_eq!(workload.restores.load(Ordering::Relaxed), 1);
        assert_eq!(workload.snapshots.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn native_worker_preserves_edge_preparation_and_shortened_keyless_terminal() {
        let spec = spec(Vec::new());
        let workload = TestWorkload::new(Mode::EdgePreparation);
        let trace = run_expansion_task(&workload, &(), &mut Target::default(), &spec, &())
            .unwrap()
            .trace
            .unwrap();
        assert!(!trace.failures.is_empty());
        for node in &trace.nodes[1..] {
            let evidence = &node.evidence.as_ref().unwrap().evidence;
            assert_eq!(evidence.preparation_failure, Some(vec![17]));
            assert!(evidence.observations.is_empty());
            assert_eq!(node.edge_cost, 0);
            assert!(node.facts.is_none());
            assert!(node.snapshot.is_none());
        }
        let workload = TestWorkload::new(Mode::Terminal);
        let trace = run_expansion_task(&workload, &(), &mut Target::default(), &spec, &())
            .unwrap()
            .trace
            .unwrap();
        assert!(trace.frontier.is_empty());
        for node in &trace.nodes[1..] {
            let evidence = &node.evidence.as_ref().unwrap().evidence;
            assert_eq!((evidence.requested_action, evidence.action), (4, 2));
            assert_eq!(node.edge_cost, 2);
            assert!(node.facts.is_none());
            assert!(node.snapshot.is_none());
            assert_eq!(evidence.observations, vec![2]);
        }
    }

    #[test]
    fn native_worker_preserves_typed_integrity_and_terminal_key_errors() {
        for mode in [Mode::Integrity, Mode::TerminalKeyError] {
            let error = run_expansion_task(
                &TestWorkload::new(mode),
                &(),
                &mut Target::default(),
                &spec(Vec::new()),
                &(),
            )
            .err()
            .unwrap();
            assert!(error.downcast_ref::<IntegrityFailure>().is_some());
        }
    }

    mod run_tests {
        use super::*;
        include!("campaign_expansion_run_tests.rs");
    }
}
