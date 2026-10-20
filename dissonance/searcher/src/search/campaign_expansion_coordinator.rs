// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::search::checkpoint::publisher::CheckpointPublisher;

#[path = "campaign_expansion_checkpoint.rs"]
mod checkpoint;

#[path = "campaign_expansion_run.rs"]
mod run;

pub use run::{ExpansionOutcome, ExpansionRunReport, run_expansion_watched};

#[path = "campaign_expansion_replay.rs"]
mod replay;

pub use replay::replay_expansion;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExpansionCounters {
    pub tasks_reserved: u64,
    pub tasks_admitted: u64,
    pub budget_consumed: u64,
    pub edge_attempts: u64,
    pub executed_edges: u64,
    pub preparation_failures: u64,
    pub empty_tasks: u64,
    pub reconstruction_work: u64,
    pub execution_work: u64,
    pub bootstrap_work: u64,
    pub executions_to_first_objective: Option<u64>,
    pub work_to_first_objective: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpansionStopReason {
    Budget,
    WorkBudget,
    Objective,
    WallBudget,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExpansionDecision {
    Filtered,
    PreparationFailed,
    Objective,
    Retained { id: u64 },
    Duplicate { id: u64 },
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExpansionEdgeAdmission {
    pub id: ExpansionEdgeId,
    pub sequence: u64,
    pub decision: ExpansionDecision,
}

pub struct ExpansionAdmission<G: ExpansionWorkload, N> {
    pub spec: ExpansionTaskSpec<G>,
    pub outcome: ExpansionTaskOutcome<G, N>,
    pub decisions: Vec<ExpansionEdgeAdmission>,
    pub useful: bool,
}

struct PendingExpansion<G: ExpansionWorkload> {
    spec: ExpansionTaskSpec<G>,
}

struct ExpansionPersistence<G: ExpansionWorkload> {
    publisher: CheckpointPublisher<G::Snapshot, <G::Key as ArchiveKey>::Progress>,
    every: Option<NonZeroU64>,
    last_sequence: u64,
}

pub struct ExpansionCoordinator<'a, G: ExpansionWorkload, N = ()> {
    workload: &'a G,
    config: &'a CampaignConfig<G>,
    core: CoordinatorCore<G>,
    rand: RomuDuoJrRand,
    counters: ExpansionCounters,
    claims: u64,
    reservations:
        ReservationSlots<u64, PendingExpansion<G>, Result<ExpansionTaskOutcome<G, N>, String>>,
    fifo: VecDeque<ReservationToken>,
    failed: bool,
    stopped: bool,
    checkpoints: Option<ExpansionPersistence<G>>,
}

fn expansion_records<G: ExpansionWorkload, N>(
    result: &Result<ExpansionTaskOutcome<G, N>, String>,
) -> (u64, u64) {
    let Ok(outcome) = result else {
        return (0, 0);
    };
    outcome.trace.as_ref().map_or((0, 0), |trace| {
        (
            trace.accounting.task_unique_edge_attempts as u64,
            trace.frontier.len() as u64,
        )
    })
}

impl<'a, G: ExpansionWorkload, N> ExpansionCoordinator<'a, G, N> {
    pub fn start(
        workload: &'a G,
        config: &'a CampaignConfig<G>,
        origin: &CampaignOrigin<G>,
    ) -> Result<Self, Box<dyn Error>> {
        workload.verify_integrity()?;
        if config.workers == 0 || config.window == 0 {
            return Err("expansion requires nonzero workers and admission window".into());
        }
        if !archive_entry_limit_is_valid(config.archive_entry_limit)
            || config.memory_budget_mib == Some(0)
        {
            return Err("expansion archive bounds are invalid".into());
        }
        if config.mixture != DrawMixture::AlphabetOnly
            || config.retention != RetentionPolicy::Unprobed
        {
            return Err("bounded expansion requires alphabet-only unprobed execution".into());
        }
        if !config.stop_rollout_on_objective {
            return Err("bounded expansion requires objective stopping inside each branch".into());
        }
        if let CampaignOrigin::SearchCheckpoint {
            path,
            expected_sha256,
        } = origin
        {
            let reader = CheckpointReader::open(path)?;
            if expected_sha256
                .as_ref()
                .is_some_and(|expected| expected != &reader.file_sha256)
            {
                return Err("selected expansion checkpoint changed before resume".into());
            }
            return with_coordinator_session(workload, || {
                checkpoint::restore(workload, config, reader)
            });
        }
        let mut core = CoordinatorCore::new(
            workload,
            &config.run,
            config.archive_entry_limit,
            config.memory_budget_mib,
        );
        let bootstrap_work = with_coordinator_session(workload, || {
            let mut target = workload.new_target()?;
            let before = workload.execution_work(&target);
            bootstrap_core(workload, &config.run, &mut core, &mut target, origin)?;
            workload.verify_integrity()?;
            execution_work_delta(
                before,
                workload.execution_work(&target),
                "expansion bootstrap",
            )
        })?;
        if !resident_memory_is_within_budget(
            core.archive.resident_memory_bytes(),
            config.memory_budget_mib,
        ) {
            return Err("expansion bootstrap exceeds its memory budget".into());
        }
        core.archive.prepare_selection();
        let bootstrap_objective = core.objective_witness.is_some();
        let capacity = config
            .window
            .min(usize::try_from(config.execution_budget).unwrap_or(usize::MAX));
        let mut fifo = VecDeque::new();
        fifo.try_reserve_exact(capacity)?;
        Ok(Self {
            workload,
            config,
            core,
            rand: RomuDuoJrRand::with_seed(derive_selection_seed(config.campaign_seed)),
            counters: ExpansionCounters {
                bootstrap_work,
                executions_to_first_objective: bootstrap_objective.then_some(0),
                work_to_first_objective: bootstrap_objective.then_some(bootstrap_work),
                ..ExpansionCounters::default()
            },
            claims: 0,
            reservations: ReservationSlots::try_new(expansion_records::<G, N>, capacity)?,
            fifo,
            failed: false,
            stopped: false,
            checkpoints: None,
        })
    }

    pub fn enable_checkpoints(&mut self, mut plan: CheckpointPlan) -> Result<(), Box<dyn Error>> {
        self.ensure_active()?;
        if self.checkpoints.is_some() {
            return Err("expansion checkpoints are already enabled".into());
        }
        let executable = self.workload.executable_identity()?;
        let every = plan.every.take();
        let publisher = CheckpointPublisher::create(
            plan,
            checkpoint::header(self.workload, self.config, &executable, ""),
            G::checkpoint_marks(&self.core.evidence),
            self.core.archive.top_progress(),
        )?;
        self.checkpoints = Some(ExpansionPersistence {
            publisher,
            every,
            last_sequence: self.core.sequence,
        });
        Ok(())
    }

    pub fn checkpoint(&mut self, reason: &str) -> Result<(), Box<dyn Error>> {
        self.ensure_active()?;
        let Some(mut persistence) = self.checkpoints.take() else {
            return Ok(());
        };
        let result = checkpoint::publish(self, &mut persistence.publisher, reason);
        self.checkpoints = Some(persistence);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn checkpoint_due(&mut self) -> Result<(), Box<dyn Error>> {
        let reason = self.checkpoints.as_mut().and_then(|persistence| {
            let reason = persistence.publisher.due(
                self.core.sequence,
                G::checkpoint_marks(&self.core.evidence),
                self.core.archive.top_progress(),
            );
            let interval = persistence.every.is_some_and(|every| {
                self.core.sequence / every.get() > persistence.last_sequence / every.get()
            });
            persistence.last_sequence = self.core.sequence;
            reason.or(interval.then_some("interval"))
        });
        if let Some(reason) = reason {
            self.checkpoint(reason)?;
        }
        Ok(())
    }

    pub fn poll_persistence(&mut self) -> Result<(), Box<dyn Error>> {
        self.ensure_active()?;
        let result = self
            .checkpoints
            .as_mut()
            .map_or(Ok(()), |persistence| persistence.publisher.poll());
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    pub fn finish_persistence<T>(
        &mut self,
        primary: Result<T, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        match self.checkpoints.as_mut() {
            Some(persistence) => persistence.publisher.finish(primary),
            None => primary,
        }
    }

    pub fn pending_tasks(&self) -> Result<Vec<ExpansionTaskSpec<G>>, Box<dyn Error>> {
        self.ensure_active()?;
        let mut specs = Vec::new();
        specs.try_reserve_exact(self.fifo.len())?;
        for token in &self.fifo {
            let slot = self
                .reservations
                .slot(*token)
                .ok_or("expansion pending FIFO token is stale")?;
            specs.push(slot.pending.spec.clone());
        }
        Ok(specs)
    }

    fn ensure_active(&self) -> Result<(), Box<dyn Error>> {
        if self.failed {
            Err("expansion coordinator is poisoned after a failure".into())
        } else {
            Ok(())
        }
    }

    pub fn reserve(&mut self) -> Result<Option<ExpansionTaskSpec<G>>, Box<dyn Error>> {
        self.ensure_active()?;
        let result = self.reserve_inner();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn reserve_inner(&mut self) -> Result<Option<ExpansionTaskSpec<G>>, Box<dyn Error>> {
        self.workload.verify_integrity()?;
        let used = self
            .counters
            .budget_consumed
            .checked_add(self.claims)
            .ok_or("expansion budget accounting overflow")?;
        if self.stopped
            || self.fifo.len() == self.config.window
            || used >= self.config.execution_budget
            || (self.config.stop_campaign_on_objective && self.core.objective_witness.is_some())
        {
            return Ok(None);
        }
        let quota = (self.config.execution_budget - used).min(expansion::MAX_EDGES as u64) as u8;
        let edge_budget = NonZeroU8::new(quota).ok_or("expansion has no remaining edge quota")?;
        let (index, selection) = self.core.archive.select_parent(&mut self.rand)?;
        let parent_id = self
            .core
            .archive
            .stable_id(index)
            .ok_or("expansion parent is missing")?;
        let parent_key = self.core.archive.entries[index].key;
        let parent_milestones = self.core.archive.entries[index].milestones;
        let root_cost_in_group = self.core.archive.entry_cost_in_group(index);
        let last_action = self.core.archive.last_action(index).copied();
        self.core.archive.pin_metadata(parent_id)?;
        let (snapshot, replay, replay_certificates, snapshot_id) =
            match self.core.archive.pin_job_origin(index) {
                Ok(parts) => parts,
                Err(error) => {
                    self.core.archive.unpin_metadata(parent_id);
                    return Err(error.into());
                }
            };
        let task = self.counters.tasks_reserved;
        let next_task = task
            .checked_add(1)
            .ok_or("expansion task identity overflow")?;
        let spec = ExpansionTaskSpec {
            task,
            parent_id,
            snapshot_id,
            snapshot,
            replay,
            replay_certificates,
            parent_key,
            parent_milestones,
            root_cost_in_group,
            last_action,
            task_seed: self.rand.next_u64(),
            edge_budget,
        };
        let pending = PendingExpansion { spec: spec.clone() };
        if self.reservations.insert(task, pending).is_some() {
            return Err("expansion task identity was reused".into());
        }
        let token = self
            .reservations
            .token(&task)
            .ok_or("expansion reservation disappeared")?;
        self.fifo.push_back(token);
        self.claims += u64::from(quota);
        self.counters.tasks_reserved = next_task;
        self.core.archive.record_selection(index, &selection);
        Ok(Some(spec))
    }

    pub fn complete(&mut self, outcome: ExpansionTaskOutcome<G, N>) -> Result<(), Box<dyn Error>> {
        self.ensure_active()?;
        let result = (|| {
            self.workload.verify_integrity()?;
            let task = outcome.task;
            let token = self
                .reservations
                .token(&task)
                .ok_or("expansion worker replied for an unknown task")?;
            self.reservations
                .complete_token(token, task, Ok(outcome), None, |_| Ok(()))?;
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    pub fn complete_error(&mut self, task: u64, error: String) -> Result<(), Box<dyn Error>> {
        self.ensure_active()?;
        let result = (|| {
            let token = self
                .reservations
                .token(&task)
                .ok_or("expansion failure names an unknown task")?;
            self.reservations
                .complete_token(token, task, Err(error), None, |_| Ok(()))?;
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    pub fn admit_next(&mut self) -> Result<Option<ExpansionAdmission<G, N>>, Box<dyn Error>> {
        self.ensure_active()?;
        let result = self.admit_next_inner();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn admit_next_inner(&mut self) -> Result<Option<ExpansionAdmission<G, N>>, Box<dyn Error>> {
        let Some(&token) = self.fifo.front() else {
            return Ok(None);
        };
        if self
            .reservations
            .slot(token)
            .is_none_or(|slot| slot.completed.is_none())
        {
            return Ok(None);
        }
        self.workload.verify_integrity()?;
        let spec = self
            .reservations
            .slot(token)
            .ok_or("expansion pending task is missing")?
            .pending
            .spec
            .clone();
        let completed = self
            .reservations
            .begin_admission(token)
            .ok_or("expansion completion disappeared")?;
        let mut outcome = match completed {
            Ok(outcome) => outcome,
            Err(error) => {
                self.core.archive.unpin_job_origin(spec.snapshot_id);
                self.core.archive.unpin_metadata(spec.parent_id);
                return Err(format!("expansion task {} failed: {error}", spec.task).into());
            }
        };
        let merged = merge_task(
            self.workload,
            &mut self.core,
            &spec,
            &mut outcome,
            &mut self.counters,
        );
        self.core.archive.unpin_job_origin(spec.snapshot_id);
        let (decisions, useful, used) = match merged {
            Ok(merged) => merged,
            Err(error) => {
                self.core.archive.unpin_metadata(spec.parent_id);
                return Err(error);
            }
        };
        let parent = self
            .core
            .archive
            .index_of_id(spec.parent_id)
            .ok_or("expansion parent disappeared during admission")?;
        self.core.archive.record_selection_outcome(parent, useful);
        self.core.archive.unpin_metadata(spec.parent_id);
        self.core.archive.maintain_memory_budget()?;
        self.core.archive.prepare_selection();
        self.claims = self
            .claims
            .checked_sub(u64::from(spec.edge_budget.get()))
            .ok_or("expansion claim underflow")?;
        self.counters.budget_consumed = self
            .counters
            .budget_consumed
            .checked_add(used)
            .ok_or("expansion budget overflow")?;
        self.counters.tasks_admitted += 1;
        self.reservations.finish_admission(token)?;
        self.fifo.pop_front();
        if useful
            && let (Some(path), Some(input)) = (
                &self.config.objective_witness_path,
                &self.core.objective_witness,
            )
        {
            std::fs::write(path, serde_json::to_vec_pretty(input)?)?;
        }
        Ok(Some(ExpansionAdmission {
            spec,
            outcome,
            decisions,
            useful,
        }))
    }

    pub fn stop_reservations(&mut self) {
        self.stopped = true;
    }

    pub fn archive(&self) -> &Archive<G::Action, G::Key, G::Milestones, G::Snapshot> {
        &self.core.archive
    }
    pub fn evidence(&self) -> &G::Evidence {
        &self.core.evidence
    }
    pub fn counters(&self) -> &ExpansionCounters {
        &self.counters
    }
    pub fn executions(&self) -> u64 {
        self.counters.executed_edges
    }
    pub fn objective_witness(&self) -> Option<&Input<G::Action>> {
        self.core.objective_witness.as_ref()
    }
    pub fn finished(&self) -> bool {
        self.fifo.is_empty()
    }
    pub fn is_stopped(&self) -> bool {
        self.finished()
            && (self.failed
                || self.stopped
                || self.counters.budget_consumed >= self.config.execution_budget
                || (self.config.stop_campaign_on_objective && self.objective_witness().is_some()))
    }
    pub fn window_len(&self) -> usize {
        self.fifo.len()
    }
}

type ExpansionOrigin<G> = (CampaignOriginRecord, Option<CampaignOrigin<G>>);

fn expansion_origin<G: ExpansionWorkload>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOrigin<G>,
    executable: &str,
) -> Result<ExpansionOrigin<G>, Box<dyn Error>> {
    let CampaignOrigin::SearchCheckpoint {
        path,
        expected_sha256,
    } = origin
    else {
        return Ok((
            resolve_origin(workload, config, origin, None, executable)?,
            None,
        ));
    };
    let reader = CheckpointReader::open(path)?;
    if expected_sha256
        .as_ref()
        .is_some_and(|expected| expected != &reader.file_sha256)
    {
        return Err("selected expansion checkpoint changed before opening".into());
    }
    let record = CampaignOriginRecord {
        kind: ORIGIN_SEARCH_CHECKPOINT.into(),
        path: None,
        archive_sha256: None,
        checkpoint_path: Some(path.display().to_string()),
        checkpoint_sha256: Some(reader.file_sha256.clone()),
        checkpoint_policy_changes: BTreeMap::new(),
        resume_input_sha256: format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&Input::<G::Action>::default())?)
        ),
        resume_actions: 0,
    };
    Ok((
        record,
        Some(CampaignOrigin::SearchCheckpoint {
            path: path.clone(),
            expected_sha256: Some(reader.file_sha256),
        }),
    ))
}

fn merge_task<G: ExpansionWorkload, N>(
    workload: &G,
    core: &mut CoordinatorCore<G>,
    spec: &ExpansionTaskSpec<G>,
    outcome: &mut ExpansionTaskOutcome<G, N>,
    counters: &mut ExpansionCounters,
) -> Result<(Vec<ExpansionEdgeAdmission>, bool, u64), Box<dyn Error>> {
    if outcome.task != spec.task || outcome.preparation_failure.is_some() == outcome.trace.is_some()
    {
        return Err("expansion outcome has an invalid root result shape".into());
    }
    if let Some(trace) = &outcome.trace {
        validate_trace(workload, spec, trace)?;
    }
    counters.reconstruction_work = counters
        .reconstruction_work
        .checked_add(outcome.reconstruction_work)
        .ok_or("expansion reconstruction work overflow")?;
    if let Some(observations) = &outcome.preparation_failure {
        workload.merge_preparation_failure(&mut core.evidence, observations);
        core.execution_failures = core.execution_failures.saturating_add(1);
        counters.preparation_failures += 1;
        counters.empty_tasks += 1;
        return Ok((Vec::new(), false, 1));
    }
    let trace = outcome.trace.as_mut().ok_or("expansion trace is missing")?;
    if trace.root.parent_id != spec.parent_id
        || trace.root.task_seed != spec.task_seed
        || trace.root.edge_budget != spec.edge_budget
        || trace.root.cost_in_group != spec.root_cost_in_group
        || trace.root.facts.key != spec.parent_key
        || trace.root.facts.milestones != spec.parent_milestones
        || trace.root.last_action != spec.last_action
        || trace.accounting.task_unique_edge_attempts > usize::from(spec.edge_budget.get())
        || trace.nodes.len() != trace.accounting.task_unique_edge_attempts + 1
    {
        return Err("expansion trace differs from its reserved root or quota".into());
    }
    let root_index = core
        .archive
        .index_of_id(spec.parent_id)
        .ok_or("expansion root is no longer resident")?;
    let retained_before = core.archive.retained;
    let mut decisions = Vec::with_capacity(trace.nodes.len().saturating_sub(1));
    for i in 1..trace.nodes.len() {
        let (prefix, rest) = trace.nodes.split_at_mut(i);
        let node = &mut rest[0];
        let edge = node
            .evidence
            .as_ref()
            .ok_or("expansion edge evidence is missing")?;
        let evidence = &edge.evidence;
        if node.suffix.last() != Some(&evidence.action)
            || node.requested_action != Some(evidence.requested_action)
        {
            return Err("expansion edge action differs from its path".into());
        }
        core.sequence = core
            .sequence
            .checked_add(1)
            .ok_or("expansion sequence overflow")?;
        let sequence = core.sequence;
        counters.edge_attempts += 1;
        counters.execution_work = counters
            .execution_work
            .checked_add(evidence.execution_work)
            .ok_or("expansion execution work overflow")?;
        if let Some(observations) = &evidence.preparation_failure {
            workload.merge_preparation_failure(&mut core.evidence, observations);
            core.execution_failures = core.execution_failures.saturating_add(1);
            counters.preparation_failures += 1;
            decisions.push(ExpansionEdgeAdmission {
                id: ExpansionEdgeId {
                    task: spec.task,
                    ordinal: node.ordinal,
                },
                sequence,
                decision: ExpansionDecision::PreparationFailed,
            });
            continue;
        }
        counters.executed_edges += 1;
        workload.merge_expansion_evidence(&mut core.evidence, evidence, sequence, || {
            let mut input = core.archive.materialize_input(root_index)?;
            input.actions.extend_from_slice(&node.suffix);
            Ok(input)
        })?;
        use crate::search::rollout::ExecutionDisposition;
        match evidence.outcome.disposition {
            ExecutionDisposition::Terminal => {
                core.terminal_endpoints = core.terminal_endpoints.saturating_add(1);
                if evidence.outcome.objective_reached {
                    core.terminal_objectives = core.terminal_objectives.saturating_add(1);
                }
            }
            ExecutionDisposition::Failed => {
                core.execution_failures = core.execution_failures.saturating_add(1);
            }
            ExecutionDisposition::Runnable => {}
        }
        let decision = if evidence.outcome.objective_reached {
            if counters.work_to_first_objective.is_none() {
                counters.work_to_first_objective = Some(
                    counters
                        .bootstrap_work
                        .checked_add(counters.reconstruction_work)
                        .and_then(|work| work.checked_add(counters.execution_work))
                        .ok_or("expansion first-objective work overflow")?,
                );
                counters.executions_to_first_objective = Some(counters.executed_edges);
            }
            core.objectives_reached = core.objectives_reached.saturating_add(1);
            if core.objective_witness.is_none() {
                let mut input = core.archive.materialize_input(root_index)?;
                input.actions.extend_from_slice(&node.suffix);
                core.objective_witness = Some(input);
            }
            ExpansionDecision::Objective
        } else if trace.frontier.contains(&node.ordinal) {
            let facts = node
                .facts
                .as_ref()
                .ok_or("expansion frontier facts are missing")?;
            if core
                .archive
                .complete_after(Some(root_index), None, facts.key)?
                != facts.key
            {
                return Err("expansion workload requires lineage-dependent key completion".into());
            }
            let mut certificates = Vec::with_capacity(node.suffix.len());
            let mut parent = node.parent_ordinal;
            while let Some(ordinal) = parent {
                if ordinal == 0 {
                    break;
                }
                let ancestor = prefix
                    .iter()
                    .find(|ancestor| ancestor.ordinal == ordinal)
                    .ok_or("expansion prefix is missing")?;
                certificates.push(
                    ancestor
                        .evidence
                        .as_ref()
                        .ok_or("expansion prefix evidence is missing")?
                        .evidence
                        .replay_certificate,
                );
                parent = ancestor.parent_ordinal;
            }
            certificates.reverse();
            certificates.push(evidence.replay_certificate);
            if certificates.len() != node.suffix.len()
                || (certificates.iter().any(Option::is_some)
                    && certificates.iter().any(Option::is_none))
            {
                return Err("expansion path has incomplete replay certificates".into());
            }
            let snapshot = node
                .snapshot
                .take()
                .ok_or("expansion frontier snapshot is missing")?;
            let snapshot = Arc::try_unwrap(snapshot)
                .map_err(|_| "expansion frontier snapshot has unexpected external owners")?;
            let before = core.archive.retained;
            let (admitted, key, unused) = core.archive.insert_after_certified_returning_unused(
                Some(root_index),
                None,
                sequence,
                ArchiveCandidate {
                    suffix: node.suffix.as_slice(),
                    key: facts.key,
                    milestones: facts.milestones,
                },
                snapshot,
                certificates
                    .iter()
                    .any(Option::is_some)
                    .then_some(certificates.as_slice()),
            )?;
            if key != facts.key {
                return Err("expansion archive key differs from the worker key".into());
            }
            node.snapshot = unused.map(Arc::new);
            match admitted {
                Some(index) => {
                    let id = core
                        .archive
                        .stable_id(index)
                        .ok_or("expansion admitted entry is missing")?;
                    if core.archive.retained > before {
                        ExpansionDecision::Retained { id }
                    } else {
                        ExpansionDecision::Duplicate { id }
                    }
                }
                None => ExpansionDecision::Rejected,
            }
        } else {
            ExpansionDecision::Filtered
        };
        decisions.push(ExpansionEdgeAdmission {
            id: ExpansionEdgeId {
                task: spec.task,
                ordinal: node.ordinal,
            },
            sequence,
            decision,
        });
        if sequence.is_multiple_of(core.curve_interval) {
            core.push_curve_point();
            core.compact_progress_curve_if_needed();
        }
    }
    let attempts = trace.accounting.task_unique_edge_attempts as u64;
    if attempts == 0 {
        counters.empty_tasks += 1;
    }
    Ok((
        decisions,
        core.archive.retained > retained_before || !trace.objectives.is_empty(),
        attempts.max(1),
    ))
}

fn validate_trace<G: ExpansionWorkload, N>(
    workload: &G,
    spec: &ExpansionTaskSpec<G>,
    trace: &ExpansionTrace<G, N>,
) -> Result<(), Box<dyn Error>> {
    if trace.root.parent_id != spec.parent_id
        || trace.root.task_seed != spec.task_seed
        || trace.root.edge_budget != spec.edge_budget
        || trace.root.cost_in_group != spec.root_cost_in_group
        || trace.root.last_action != spec.last_action
        || trace.root.facts.key != spec.parent_key
        || trace.root.facts.milestones != spec.parent_milestones
        || trace.root.facts.progress != spec.parent_key.progress()
        || trace.root.facts.quality != workload.expansion_quality(spec.parent_key)
        || trace.root.facts.bucket != workload.expansion_bucket(spec.parent_key)
        || trace.nodes.len() > expansion::MAX_EDGES + 1
        || trace.accounting.task_unique_edge_attempts > usize::from(spec.edge_budget.get())
        || trace.nodes.len() != trace.accounting.task_unique_edge_attempts + 1
        || trace.frontier.len() > expansion::WIDTH
    {
        return Err("expansion trace differs from its reserved root or quota".into());
    }
    let root = trace
        .nodes
        .first()
        .ok_or("expansion trace lacks its root")?;
    if root.ordinal != 0
        || root.parent_ordinal.is_some()
        || root.depth != 0
        || !root.suffix.is_empty()
        || root.requested_action.is_some()
        || root.evidence.is_some()
        || root.path_cost != 0
        || root.edge_cost != 0
        || root.cost_in_group != Some(spec.root_cost_in_group)
        || root.status != expansion::Status::Runnable
        || root.objective
    {
        return Err("expansion trace has an invalid root node".into());
    }
    for (i, node) in trace.nodes.iter().enumerate().skip(1) {
        let parent_ordinal = node.parent_ordinal.ok_or("expansion edge lacks a parent")?;
        let parent = trace.nodes[..i]
            .iter()
            .find(|parent| parent.ordinal == parent_ordinal)
            .ok_or("expansion edge parent does not precede its child")?;
        let edge = node
            .evidence
            .as_ref()
            .ok_or("expansion edge lacks evidence")?;
        let evidence = &edge.evidence;
        let expected_status = match evidence.outcome.disposition {
            crate::search::rollout::ExecutionDisposition::Runnable => expansion::Status::Runnable,
            crate::search::rollout::ExecutionDisposition::Terminal => expansion::Status::Terminal,
            crate::search::rollout::ExecutionDisposition::Failed => expansion::Status::Failed,
        };
        if node.ordinal <= trace.nodes[i - 1].ordinal
            || node.ordinal > (1 << (expansion::DEPTH + 1)) - 2
            || (node.ordinal - 1) / 2 != parent_ordinal
            || node.depth != parent.depth + 1
            || node.depth > expansion::DEPTH
            || parent.status != expansion::Status::Runnable
            || parent.objective
            || node.suffix.len() != parent.suffix.len() + 1
            || !node.suffix.starts_with(&parent.suffix)
            || node.suffix.last() != Some(&evidence.action)
            || node.requested_action != Some(evidence.requested_action)
            || node.status != expected_status
            || node.objective != evidence.outcome.objective_reached
            || (node.status == expansion::Status::Runnable
                && evidence.action != evidence.requested_action)
            || (node.status == expansion::Status::Runnable && node.facts.is_none())
            || (node.status == expansion::Status::Failed
                && (node.facts.is_some() || node.snapshot.is_some() || node.objective))
            || !workload.validate_expansion_action(
                evidence.requested_action,
                evidence.action,
                evidence.outcome.disposition,
            )
        {
            return Err("expansion trace has an invalid tree edge".into());
        }
        if let Some(facts) = &node.facts
            && (facts.progress != facts.key.progress()
                || facts.quality != workload.expansion_quality(facts.key)
                || facts.bucket != workload.expansion_bucket(facts.key)
                || facts.milestones != evidence.milestones
                || node.cost_in_group
                    != Some(expansion::root_relative_cost(
                        &spec.parent_key.progress(),
                        spec.root_cost_in_group,
                        &facts.progress,
                        node.path_cost,
                    )))
        {
            return Err("expansion node facts differ from their workload policy".into());
        }
        if node.path_cost
            != parent
                .path_cost
                .checked_add(node.edge_cost)
                .ok_or("expansion path cost overflow")?
            || (evidence.preparation_failure.is_none()
                && node.edge_cost != (workload.action_cost_fn())(&evidence.action))
            || (evidence.preparation_failure.is_some()
                && (node.status != expansion::Status::Failed
                    || node.edge_cost != 0
                    || !evidence.observations.is_empty()
                    || node.objective))
        {
            return Err("expansion edge cost or preparation failure is invalid".into());
        }
        if node.snapshot.is_some() && !trace.frontier.contains(&node.ordinal) {
            return Err("expansion retained a snapshot outside its frontier".into());
        }
    }
    let expected_objectives: Vec<_> = trace
        .nodes
        .iter()
        .skip(1)
        .filter(|node| node.objective)
        .map(|node| node.ordinal)
        .collect();
    let expected_failures: Vec<_> = trace
        .nodes
        .iter()
        .skip(1)
        .filter(|node| node.status == expansion::Status::Failed)
        .map(|node| node.ordinal)
        .collect();
    if expected_objectives != trace.objectives || expected_failures != trace.failures {
        return Err("expansion endpoint indexes differ from their nodes".into());
    }
    for (i, ordinal) in trace.frontier.iter().enumerate() {
        let node = trace
            .nodes
            .iter()
            .find(|node| node.ordinal == *ordinal)
            .ok_or("expansion frontier node is missing")?;
        if *ordinal == 0
            || trace.frontier[..i].contains(ordinal)
            || node.status != expansion::Status::Runnable
            || node.objective
            || node.facts.is_none()
            || node.snapshot.is_none()
            || node
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| Arc::strong_count(snapshot) != 1)
        {
            return Err("expansion frontier has an invalid endpoint".into());
        }
    }
    Ok(())
}
