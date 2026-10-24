// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::Write,
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};

use super::{
    CONSECUTIVE_SKIP_LIMIT, CampaignAdmissionDecision, CampaignCancelRecord, CampaignConfig,
    CampaignCounters, CampaignEndRecord, CampaignExecutionOptions, CampaignJobRecord,
    CampaignJobResult, CampaignOrigin, CampaignOriginRecord, CampaignOutcome,
    CampaignProgressRecord, CampaignSkipRecord, CampaignSpliceRecord, CampaignStreamHeader,
    CampaignStreamRecord, CampaignTypes, CompletedBuffer, CompletedJob, CoordinatorCore,
    JobSpecRecord, LONGEST_SPLICE_TAIL, LiveCoordinatorProfile, PendingJob, SelectedJob,
    StreamWriter, Workload, archive_entry_limit_is_valid, bootstrap_campaign, build_report,
    completed_job_records, continuation_arrival, continuation_attempt, derive_selection_seed,
    execution_work_delta, finish_record, initial_draw_state, memory_is_within_reserve,
    mixture_weights, planned_finish, profile_elapsed, profile_now, progress_checkpoint_due,
    progress_record, record_compaction_elapsed, record_mixture_outcome, resolve_origin,
    restore_search_checkpoint, retained_archive_indexes, search_checkpoint_header,
    stop_reservations_after_objective, stop_reservations_after_work_budget, stream_header,
    with_coordinator_session, write_live_progress, write_search_checkpoint,
};
use crate::search::archive::{
    Archive, ArchiveKey, CampaignSpliceTail, Input, SelectorDraw, SelectorPath,
};
use crate::search::checkpoint::{CheckpointReader, publisher::CheckpointPublisher};
use crate::search::draw::{EnergyStrategy, MixtureDraw, energy_strategy};
use crate::search::draw_tables::{DrawTableHeader, DrawTables};
use crate::search::duration::DurationPolicies;
use crate::search::empirical_steps::EmpiricalStepCheckpoint;
use crate::search::rand::RomuDuoJrRand;
use crate::search::rollout::RolloutStop;
use crate::search::telemetry::{
    CampaignTelemetry, HostTimes, IdleClock, IdleReason, Moment, WorkerTelemetry, nanos_since, now,
    thread_schedstat,
};

#[derive(Deserialize, Serialize)]
#[serde(bound = "")]
pub struct JobSpec<G: CampaignTypes + ?Sized> {
    pub reservation: usize,
    pub planned_finish: u64,
    pub snapshot: Arc<G::Snapshot>,
    pub replay: Vec<G::Action>,
    pub replay_certificates: Vec<Option<[u8; 32]>>,
    pub parent_milestones: G::Milestones,
    pub suffix: Vec<G::Action>,
}

#[derive(Deserialize, Serialize)]
#[serde(bound = "")]
pub struct JobOutcome<G: CampaignTypes + ?Sized> {
    pub reservation: usize,
    pub result: CampaignJobResult<G>,
    pub execution_work: u64,
    pub result_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CampaignAdmission {
    pub reservation: usize,
    pub decisions: Vec<CampaignAdmissionDecision>,
}

pub fn run_job<G: Workload>(
    workload: &G,
    config: &CampaignConfig<G>,
    target: &mut G::Target,
    job: &JobSpec<G>,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<JobOutcome<G>, Box<dyn Error>> {
    let stop = RolloutStop::new(config.stop_rollout_on_objective).with_cancel(cancel);
    let work_before = workload.execution_work(target);
    let result = workload.execute_job_certified(
        &config.run,
        target,
        &job.snapshot,
        &job.replay,
        &job.replay_certificates,
        job.parent_milestones,
        &job.suffix,
        config.retention,
        stop,
    )?;
    workload.verify_integrity()?;
    let result_sha256 = workload.certified_result_sha256(&result, target)?;
    let execution_work =
        execution_work_delta(work_before, workload.execution_work(target), "worker")?;
    Ok(JobOutcome {
        reservation: job.reservation,
        result,
        execution_work,
        result_sha256,
    })
}

pub(crate) type WorkerResult<G> = Result<(CampaignJobResult<G>, u64, String), String>;

pub struct CampaignCoordinator<'a, G: Workload> {
    workload: &'a G,
    config: &'a CampaignConfig<G>,
    work_budget: Option<u64>,
    header: CampaignStreamHeader<DrawTableHeader>,
    origin_record: CampaignOriginRecord,
    writer: StreamWriter<'a>,
    progress: Option<&'a mut dyn Write>,
    core: CoordinatorCore<G>,
    counters: CampaignCounters,
    rand: RomuDuoJrRand,
    draw_state: DrawTables<G::Action>,
    duration_policies: DurationPolicies<G::Key>,
    checkpoints: Option<CheckpointPublisher<G::Snapshot, <G::Key as ArchiveKey>::Progress>>,
    failed: bool,
    reserved: u64,
    admitted: usize,
    planned_clock: u64,
    pending: BTreeMap<usize, PendingJob<G>>,
    completed: CompletedBuffer<usize, CompletedJob<G>>,
    schedule: BTreeSet<(u64, usize)>,
    queued: BTreeMap<(u64, usize), JobSpec<G>>,
    spec_records: BTreeMap<usize, JobSpecRecord<G>>,
    running: usize,
    telemetry: CampaignTelemetry,
    profile: LiveCoordinatorProfile,
    profile_printed: bool,
    idle_clock: IdleClock,
    coordinator_schedstat: Option<(u64, u64)>,
    telemetry_started: Moment,
    wall_started: Option<Moment>,
    admissions: Option<Vec<CampaignAdmission>>,
    cancel: Arc<AtomicBool>,
    interrupt: Option<&'static AtomicBool>,
    stopped: bool,
    interrupted: bool,
    jobs_cancelled: u64,
}

impl<'a, G: Workload> CampaignCoordinator<'a, G>
where
    G::ArchiveReport: Serialize,
{
    #[allow(clippy::too_many_lines)]
    pub fn start(
        workload: &'a G,
        config: &'a CampaignConfig<G>,
        origin: &CampaignOrigin<G>,
        stream: &'a mut dyn Write,
        progress: Option<&'a mut dyn Write>,
        options: CampaignExecutionOptions,
    ) -> Result<Self, Box<dyn Error>> {
        workload.verify_integrity()?;
        let executable_identity = workload.executable_identity()?;
        let work_budget = options.work_budget;
        let interrupt = options.interrupt;
        let duration_memory_reserve = DurationPolicies::<G::Key>::memory_reserve_bytes();
        if work_budget == Some(0) {
            return Err("work budget must be nonzero".into());
        }
        if config.workers == 0 {
            return Err("campaign mode requires at least one worker".into());
        }
        if !archive_entry_limit_is_valid(config.archive_entry_limit) {
            return Err("campaign archive entry limit is outside its bounded range".into());
        }
        if config.window == 0 {
            return Err("campaign admission window must hold at least one reservation".into());
        }
        if config.memory_budget_mib == Some(0) {
            return Err("campaign memory budget must be nonzero".into());
        }
        if let Some(memory_budget_mib) = config.memory_budget_mib {
            let budget = memory_budget_mib.saturating_mul(1024 * 1024);
            if budget
                <= workload
                    .draw_state_memory_reserve_bytes(&config.run)
                    .saturating_add(duration_memory_reserve)
            {
                return Err(
                    "campaign memory budget is too small for the bounded draw state".into(),
                );
            }
        }
        let search_checkpoint = match origin {
            CampaignOrigin::SearchCheckpoint {
                path,
                expected_sha256,
            } => {
                let reader = CheckpointReader::open(path)?;
                if expected_sha256
                    .as_deref()
                    .is_some_and(|expected| expected != reader.file_sha256.as_str())
                {
                    return Err("selected search checkpoint changed before resume".into());
                }
                Some(reader)
            }
            _ => None,
        };
        let origin_record = resolve_origin(
            workload,
            config,
            origin,
            search_checkpoint.as_ref(),
            &executable_identity,
        )?;
        let draw_origin = match origin {
            CampaignOrigin::Genesis
            | CampaignOrigin::SnapshotRoot { .. }
            | CampaignOrigin::SearchCheckpoint { .. } => None,
            CampaignOrigin::Archive { source, .. } => Some((source.file_sha256(), source.report())),
        };
        let (mut draw_state, draw_header) = initial_draw_state(workload, &config.run, draw_origin)?;
        let mut duration_policies = DurationPolicies::<G::Key>::new();
        if !memory_is_within_reserve(
            workload.draw_state_memory_bytes(&draw_state),
            workload.draw_state_memory_reserve_bytes(&config.run),
        ) {
            return Err("initial draw state exceeds its deterministic memory reserve".into());
        }
        let mut header = stream_header(
            workload,
            config,
            &origin_record,
            draw_header,
            &executable_identity,
        );
        header.work_budget = work_budget;
        let mut writer = StreamWriter::new(stream);
        writer.write_line(&header)?;

        let mut core = CoordinatorCore::new(
            workload,
            &config.run,
            config.archive_entry_limit,
            config.memory_budget_mib,
        );
        let mut counters = CampaignCounters::new();
        let mut telemetry = CampaignTelemetry::default();
        let mut rand = RomuDuoJrRand::with_seed(derive_selection_seed(config.campaign_seed));

        let bootstrap_started = now();
        let resume = with_coordinator_session(workload, || {
            Ok(match search_checkpoint {
                Some(reader) => Some(restore_search_checkpoint(
                    workload,
                    config,
                    &executable_identity,
                    reader,
                    &mut core,
                    &mut draw_state,
                    &mut duration_policies,
                    &mut counters,
                )?),
                None => {
                    bootstrap_campaign(
                        workload,
                        config,
                        origin,
                        &mut core,
                        &draw_state,
                        &duration_policies,
                        &mut counters,
                    )?;
                    None
                }
            })
        })?;
        telemetry.bootstrap_ns = nanos_since(bootstrap_started);
        if let (Some(path), Some(input)) = (&config.objective_witness_path, &core.objective_witness)
        {
            std::fs::write(path, serde_json::to_vec_pretty(input)?)?;
        }

        let telemetry_started = now();
        let mut reserved = 0_u64;
        let mut admitted = 0_usize;
        let mut planned_clock = 0_u64;
        let mut profile = LiveCoordinatorProfile::default();
        let mut in_flight = Vec::new();
        if let Some(resume) = resume {
            rand = resume.rand;
            reserved = resume.reserved;
            profile = resume.profile;
            admitted = resume.admitted;
            planned_clock = resume.planned_clock;
            in_flight = resume.in_flight;
        }
        let checkpoints = options
            .checkpoints
            .map(|plan| {
                CheckpointPublisher::create(
                    plan,
                    search_checkpoint_header(
                        workload,
                        config,
                        &executable_identity,
                        "",
                        0,
                        0,
                        0,
                        0,
                    ),
                    G::checkpoint_marks(&core.evidence),
                    core.archive.top_progress(),
                )
            })
            .transpose()?;
        let mut coordinator = Self {
            workload,
            config,
            work_budget,
            header,
            origin_record,
            writer,
            progress,
            core,
            counters,
            rand,
            draw_state,
            duration_policies,
            checkpoints,
            failed: false,
            reserved,
            admitted,
            planned_clock,
            pending: BTreeMap::new(),
            completed: CompletedBuffer::new(completed_job_records),
            schedule: BTreeSet::new(),
            queued: BTreeMap::new(),
            spec_records: BTreeMap::new(),
            running: 0,
            telemetry,
            profile,
            profile_printed: std::env::var_os("HARMONY_COORDINATOR_PROFILE").is_some(),
            idle_clock: IdleClock::new(config.workers as usize),
            coordinator_schedstat: thread_schedstat(),
            telemetry_started,
            wall_started: config.wall_budget.map(|_| telemetry_started),
            admissions: None,
            cancel: Arc::new(AtomicBool::new(false)),
            interrupt,
            stopped: false,
            interrupted: false,
            jobs_cancelled: 0,
        };
        for job in in_flight {
            let parent_index = coordinator
                .core
                .archive
                .index_of_id(job.pending.parent_id)
                .ok_or("search checkpoint in-flight job names a missing parent entry")?;
            let (expected_replay, expected_certificates) = coordinator
                .core
                .archive
                .pinned_job_replay(parent_index, job.pending.snapshot_id)?;
            if job.spec.replay != expected_replay
                || job.spec.replay_certificates != expected_certificates
            {
                return Err(
                    "search checkpoint in-flight replay certificate path differs from its archive"
                        .into(),
                );
            }
            let index = coordinator
                .core
                .archive
                .index_of_id(job.pending.snapshot_id)
                .ok_or("search checkpoint in-flight job names a missing snapshot entry")?;
            let snapshot = coordinator.core.archive.entries[index]
                .snapshot
                .clone()
                .ok_or("search checkpoint in-flight job has no stored snapshot")?;
            coordinator
                .core
                .start_suffix_job(job.pending.parent_id, job.pending.suffix_limit);
            let key = (job.pending.planned_finish, job.reservation);
            coordinator.schedule.insert(key);
            coordinator.queued.insert(
                key,
                JobSpec {
                    reservation: job.reservation,
                    planned_finish: job.pending.planned_finish,
                    snapshot,
                    replay: job.spec.replay.clone(),
                    replay_certificates: job.spec.replay_certificates.clone(),
                    parent_milestones: job.spec.parent_milestones,
                    suffix: job.spec.suffix.clone(),
                },
            );
            if coordinator.checkpoints.is_some() {
                coordinator.spec_records.insert(job.reservation, job.spec);
            }
            coordinator.pending.insert(job.reservation, job.pending);
        }
        Ok(coordinator)
    }

    #[must_use]
    pub fn cancellation(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    #[must_use]
    pub const fn stopped(&self) -> bool {
        self.stopped
    }

    pub fn stop_if_due(&mut self) -> Result<bool, Box<dyn Error>> {
        if self.stopped {
            return Ok(true);
        }
        if !self.stop_due() {
            return Ok(false);
        }
        let result = self.stop_in_flight(self.interrupt_requested());
        self.handback_on_error(result)?;
        Ok(true)
    }

    fn stop_in_flight(&mut self, interrupted: bool) -> Result<(), Box<dyn Error>> {
        if interrupted {
            eprintln!("search interrupted; dropping the jobs in flight and writing the report");
        }
        self.stopped = true;
        self.interrupted = interrupted;
        self.cancel.store(true, Ordering::Release);
        self.schedule.clear();
        self.queued.clear();
        self.spec_records.clear();
        let mut dropped = std::mem::take(&mut self.pending);
        let held = self.completed.keys().copied().collect::<Vec<_>>();
        for reservation in held {
            if let Some(completed) = self.completed.remove(&reservation) {
                dropped.insert(reservation, completed.pending);
            }
        }
        for (reservation, job) in dropped {
            self.core.finish_suffix_job(job.parent_id, job.suffix_limit);
            self.core.archive.unpin_job_origin(job.snapshot_id);
            self.core.archive.unpin_metadata(job.parent_id);
            self.writer.write_line(
                &CampaignStreamRecord::<EmpiricalStepCheckpoint, G::Key>::Cancel(
                    CampaignCancelRecord {
                        reservation: u64::try_from(reservation)?,
                        parent_id: job.parent_id,
                        mutation_seed: job.mutation_seed,
                        splice: job.splice,
                        selector: job.selector,
                        suffix_limit: job.suffix_limit,
                    },
                ),
            )?;
            self.jobs_cancelled = self.jobs_cancelled.saturating_add(1);
        }
        self.idle_clock.update(self.running, IdleReason::NoJob);
        Ok(())
    }

    pub fn reserve_next(&mut self) -> Result<bool, Box<dyn Error>> {
        let result = (|| {
            self.ensure_active()?;
            self.poll_persistence()?;
            if self.stopped || self.pending.len() >= self.config.window {
                return Ok(false);
            }
            self.reserve()
        })();
        self.handback_on_error(result)
    }

    pub fn plan(&mut self, limit: usize) -> Vec<JobSpec<G>> {
        if self.failed || self.stopped {
            return Vec::new();
        }
        let mut jobs = Vec::new();
        while jobs.len() < limit
            && let Some((_, job)) = self.queued.pop_first()
        {
            jobs.push(job);
        }
        self.running = self.running.saturating_add(jobs.len());
        self.idle_clock.update(
            self.running,
            if self.completed.is_empty() {
                IdleReason::NoJob
            } else {
                IdleReason::AdmissionOrder
            },
        );
        jobs
    }

    pub fn admit(&mut self, outcome: JobOutcome<G>) -> Result<(), Box<dyn Error>> {
        let JobOutcome {
            reservation,
            result,
            execution_work,
            result_sha256,
        } = outcome;
        self.receive(reservation, Ok((result, execution_work, result_sha256)))
    }

    pub fn fail(&mut self, reservation: usize, error: String) -> Result<(), Box<dyn Error>> {
        self.receive(reservation, Err(error))
    }

    #[must_use]
    pub fn next_admission(&self) -> Option<usize> {
        self.schedule.first().map(|(_, reservation)| *reservation)
    }

    pub(crate) fn pending_origin(&self, reservation: usize) -> Option<(u64, u64)> {
        self.pending
            .get(&reservation)
            .map(|job| (job.parent_id, job.snapshot_id))
    }

    fn receive(
        &mut self,
        reservation: usize,
        outcome: WorkerResult<G>,
    ) -> Result<(), Box<dyn Error>> {
        let result = (|| {
            self.ensure_active()?;
            self.poll_persistence()?;
            self.profile.receives = self.profile.receives.saturating_add(1);
            self.running = self
                .running
                .checked_sub(1)
                .ok_or("campaign worker replied without queued work")?;
            if self.stopped || self.stop_due() {
                if !self.stopped {
                    self.stop_in_flight(self.interrupt_requested())?;
                }
                return Ok(());
            }
            let held = self.schedule.first().map(|(_, next)| *next) != Some(reservation);
            self.idle_clock.update(
                self.running,
                if held {
                    IdleReason::AdmissionOrder
                } else {
                    IdleReason::NoJob
                },
            );
            let pending = self
                .pending
                .remove(&reservation)
                .ok_or("campaign worker replied for an unknown reservation")?;
            if self
                .queued
                .contains_key(&(pending.planned_finish, reservation))
            {
                return Err("campaign worker replied for a job that was never planned".into());
            }
            if self
                .completed
                .insert(
                    reservation,
                    CompletedJob {
                        held_since: held.then(now),
                        pending,
                        outcome,
                    },
                )
                .is_some()
            {
                return Err("campaign worker completed one reservation twice".into());
            }
            while let Some(&(finish, reservation)) = self.schedule.first() {
                self.workload.verify_integrity()?;
                let Some(completed) = self.completed.remove(&reservation) else {
                    break;
                };
                self.schedule.pop_first();
                self.admit_completed(finish, reservation, completed)?;
                if stop_reservations_after_objective(
                    self.config.stop_campaign_on_objective,
                    self.core.objective_witness.is_some(),
                ) {
                    self.stop_in_flight(false)?;
                    break;
                }
                self.reserve()?;
                self.write_due_checkpoint()?;
            }
            if self.pending.is_empty() && !self.completed.is_empty() {
                return Err("campaign reorder window ended with an admission gap".into());
            }
            Ok(())
        })();
        self.handback_on_error(result)
    }

    fn interrupt_requested(&self) -> bool {
        self.interrupt
            .is_some_and(|interrupt| interrupt.load(Ordering::Acquire))
    }

    fn stop_due(&self) -> bool {
        self.interrupt_requested()
            || self
                .wall_started
                .zip(self.config.wall_budget)
                .is_some_and(|(started, budget)| started.elapsed() >= budget)
    }

    fn ensure_active(&self) -> Result<(), Box<dyn Error>> {
        self.workload.verify_integrity()?;
        if self.failed {
            return Err("campaign coordinator has already failed".into());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn checkpoint_worker_finished(&self) -> bool {
        self.checkpoints
            .as_ref()
            .is_some_and(CheckpointPublisher::worker_finished)
    }

    #[cfg(test)]
    pub(super) fn checkpoint_capture_overflows(&self) -> usize {
        self.checkpoints
            .as_ref()
            .map_or(0, CheckpointPublisher::capture_overflows)
    }

    fn poll_persistence(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(checkpoints) = self.checkpoints.as_mut() {
            checkpoints.poll()?;
        }
        Ok(())
    }

    fn handback_on_error<T>(
        &mut self,
        result: Result<T, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        if result.is_err() {
            self.finish_persistence(result)
        } else {
            result
        }
    }

    pub(crate) fn finish_persistence<T>(
        &mut self,
        result: Result<T, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        let result = match self.checkpoints.as_mut() {
            Some(checkpoints) => checkpoints.finish(result),
            None => result,
        };
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    pub fn watch_admissions(&mut self) {
        self.admissions.get_or_insert_with(Vec::new);
    }

    pub fn unwatch_admissions(&mut self) {
        self.admissions = None;
    }

    pub fn take_admissions(&mut self) -> Vec<CampaignAdmission> {
        self.admissions
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    #[must_use]
    pub fn finished(&self) -> bool {
        self.pending.is_empty() && self.completed.is_empty()
    }

    #[must_use]
    pub fn archive(&self) -> &Archive<G::Action, G::Key, G::Milestones, G::Snapshot> {
        &self.core.archive
    }

    #[must_use]
    pub fn evidence(&self) -> &G::Evidence {
        &self.core.evidence
    }

    #[must_use]
    pub const fn executions(&self) -> u64 {
        self.core.sequence
    }

    #[must_use]
    pub const fn reserved(&self) -> u64 {
        self.reserved
    }

    #[must_use]
    pub const fn objective_witness(&self) -> Option<&Input<G::Action>> {
        self.core.objective_witness.as_ref()
    }

    pub fn progress(&self) -> Result<CampaignProgressRecord<G::Key>, Box<dyn Error>> {
        progress_record(
            &self.core,
            &self.counters,
            None,
            self.workload.draw_state_memory_bytes(&self.draw_state),
            self.telemetry_started,
            false,
        )
    }

    pub(crate) fn note_receive_wait(&mut self, waited: u128) {
        self.profile.receive_wait_ns = self.profile.receive_wait_ns.saturating_add(waited);
        let waited = u64::try_from(waited).unwrap_or(u64::MAX);
        let admission = &mut self.telemetry.admission;
        if self.completed.is_empty() {
            admission.receive_all_running_ns =
                admission.receive_all_running_ns.saturating_add(waited);
        } else {
            admission.receive_head_of_line_ns =
                admission.receive_head_of_line_ns.saturating_add(waited);
        }
    }

    pub fn finish(
        mut self,
        workers: Vec<WorkerTelemetry>,
    ) -> Result<CampaignOutcome<G>, Box<dyn Error>> {
        let result = (|| {
            self.ensure_active()?;
            if !self.completed.is_empty() || !self.schedule.is_empty() {
                return Err("campaign reorder window ended with an admission gap".into());
            }
            self.telemetry.completed_buffer = self.completed.telemetry;
            if let Some(checkpoints) = self.checkpoints.as_mut() {
                write_search_checkpoint(
                    self.workload,
                    checkpoints,
                    "final",
                    &self.core,
                    &self.draw_state,
                    &self.duration_policies,
                    &self.rand,
                    &self.counters,
                    &self.profile,
                    self.reserved.saturating_sub(self.jobs_cancelled),
                    self.admitted,
                    self.planned_clock,
                    &self.schedule,
                    &self.pending,
                    &self.completed,
                    &self.spec_records,
                )?;
            }
            Ok(())
        })();
        self.finish_persistence(result)?;
        let Self {
            workload,
            config,
            header,
            origin_record,
            mut writer,
            progress,
            mut core,
            mut counters,
            reserved,
            draw_state,
            duration_policies,
            mut telemetry,
            profile,
            profile_printed,
            idle_clock,
            coordinator_schedstat,
            telemetry_started,
            interrupted,
            jobs_cancelled,
            ..
        } = self;
        telemetry.search_ns = nanos_since(telemetry_started);
        telemetry.workers = workers;
        idle_clock.finish(&mut telemetry.admission);
        telemetry.sum_targets();
        telemetry.coordinator = serde_json::to_value(&profile)?;
        let persistence_started = now();

        core.archive.preserve_inactive_snapshots(false)?;
        core.archive.compact_history_for_final_report()?;
        core.finish_curve();
        if let Some(sink) = progress {
            write_live_progress(
                &core,
                &counters,
                profile_printed.then(|| {
                    (
                        &profile,
                        HostTimes::measure(
                            coordinator_schedstat,
                            (
                                telemetry.admission.idle_admission_order_ns,
                                telemetry.admission.idle_no_job_ns,
                            ),
                        ),
                    )
                }),
                workload
                    .draw_state_memory_bytes(&draw_state)
                    .saturating_add(duration_policies.memory_bytes()),
                telemetry_started,
                true,
                sink,
            )?;
        }
        workload.verify_integrity()?;
        writer.write_line(
            &CampaignStreamRecord::<EmpiricalStepCheckpoint, G::Key>::End(CampaignEndRecord {
                jobs: reserved,
                skips: counters.duplicates_skipped,
                executions_completed: core.sequence,
                jobs_cancelled,
                interrupted,
            }),
        )?;
        let stream_sha256 = writer.finish()?;
        counters.draw_state_memory_bytes = workload
            .draw_state_memory_bytes(&draw_state)
            .saturating_add(duration_policies.memory_bytes());
        let (mut report, checkpoint) = build_report(
            workload,
            &header,
            origin_record,
            core,
            &counters,
            stream_sha256,
            config.materialize_final_artifacts,
        )?;
        telemetry.persistence_ns = nanos_since(persistence_started);
        report.telemetry = telemetry;
        report.jobs_cancelled = jobs_cancelled;
        report.interrupted = interrupted;
        Ok((report, checkpoint))
    }

    fn reserve(&mut self) -> Result<bool, Box<dyn Error>> {
        let selection_started = profile_now();
        let selected = self.select()?;
        self.profile.selection_ns = self
            .profile
            .selection_ns
            .saturating_add(profile_elapsed(selection_started));
        self.profile.selections = self.profile.selections.saturating_add(1);
        let Some((mut spec, mut pending)) = selected else {
            return Ok(false);
        };
        let action_cost = self.workload.action_cost_fn();
        self.profile.note_dispatch(
            &spec,
            matches!(pending.splice, Some(CampaignSpliceRecord::Tail { .. })),
            action_cost,
        );
        let reservation = usize::try_from(self.reserved.saturating_sub(1))?;
        spec.reservation = reservation;
        pending.planned_finish = planned_finish(self.planned_clock, &spec, action_cost);
        spec.planned_finish = pending.planned_finish;
        let key = (pending.planned_finish, reservation);
        if self.pending.insert(reservation, pending).is_some() {
            return Err("campaign reserved one job twice".into());
        }
        if self.checkpoints.is_some() {
            self.spec_records
                .insert(reservation, JobSpecRecord::of(&spec));
        }
        self.schedule.insert(key);
        self.queued.insert(key, spec);
        Ok(true)
    }

    fn write_due_checkpoint(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(checkpoints) = self.checkpoints.as_mut()
            && let Some(reason) = checkpoints.due(
                self.core.sequence,
                G::checkpoint_marks(&self.core.evidence),
                self.core.archive.top_progress(),
            )
        {
            write_search_checkpoint(
                self.workload,
                checkpoints,
                reason,
                &self.core,
                &self.draw_state,
                &self.duration_policies,
                &self.rand,
                &self.counters,
                &self.profile,
                self.reserved.saturating_sub(self.jobs_cancelled),
                self.admitted,
                self.planned_clock,
                &self.schedule,
                &self.pending,
                &self.completed,
                &self.spec_records,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn admit_completed(
        &mut self,
        finish: u64,
        reservation: usize,
        completed_job: CompletedJob<G>,
    ) -> Result<(), Box<dyn Error>> {
        let workload = self.workload;
        let config = self.config;
        let duration_memory_reserve = DurationPolicies::<G::Key>::memory_reserve_bytes();
        let Self {
            core,
            counters,
            draw_state,
            duration_policies,
            writer,
            progress,
            planned_clock,
            admitted,
            spec_records,
            telemetry,
            profile: coordinator_profile,
            profile_printed,
            idle_clock,
            coordinator_schedstat,
            telemetry_started,
            completed,
            admissions,
            ..
        } = self;
        let admission = &mut telemetry.admission;
        *planned_clock = finish;
        spec_records.remove(&reservation);
        let pending_job = completed_job.pending;
        let (result, execution_work, result_sha256) =
            completed_job.outcome.map_err(|error| -> Box<dyn Error> {
                format!("campaign reservation {reservation} failed: {error}").into()
            })?;
        if let Some(since) = completed_job.held_since {
            let held = nanos_since(since);
            admission.results_held = admission.results_held.saturating_add(1);
            admission.results_held_ns = admission.results_held_ns.saturating_add(held);
            admission.results_held_max_ns = admission.results_held_max_ns.max(held);
        }
        let tracked_duration = pending_job
            .duration_draw
            .filter(|_| execution_work > 0)
            .map(|draw| draw.duration);
        let duration_checkpoint_before = pending_job
            .duration_draw
            .and_then(|draw| duration_policies.context_checkpoint(draw.context));
        let objectives_before = core.objectives_reached;
        let stopped_before = core.hang_stop.is_some();
        let admission_started = profile_now();
        core.archive
            .set_admission_wave(pending_job.continuation_wave);
        core.finish_suffix_job(pending_job.parent_id, pending_job.suffix_limit);
        let (sequence, decisions, duration_admission) = core.admit_job_tracking(
            workload,
            pending_job.parent_id,
            result,
            pending_job.suffix_limit.is_some(),
            |action| {
                tracked_duration.is_some_and(|duration| {
                    workload.duration_of_action(&config.run, action) == Some(duration)
                })
            },
        )?;
        if duration_admission.applied {
            let draw = pending_job
                .duration_draw
                .ok_or("duration application was reported without a duration draw")?;
            duration_policies.observe(
                draw.context,
                draw.duration,
                duration_admission.useful,
                NonZeroU64::new(execution_work)
                    .ok_or("duration application requires positive execution work")?,
            )?;
        }
        let duration_checkpoint_after = pending_job
            .duration_draw
            .and_then(|draw| duration_policies.context_checkpoint(draw.context));
        coordinator_profile.admission_ns = coordinator_profile
            .admission_ns
            .saturating_add(profile_elapsed(admission_started));
        coordinator_profile.admissions = coordinator_profile.admissions.saturating_add(1);
        let bookkeeping_started = profile_now();
        let parent_index = core
            .archive
            .index_of_id(pending_job.parent_id)
            .ok_or("completed job parent is no longer resident")?;
        let isolated_continuation = pending_job.selector.path == SelectorPath::Continuation;
        if isolated_continuation {
            core.archive.record_isolated_continuation(parent_index);
        } else {
            core.archive
                .record_selection(parent_index, &pending_job.selector);
        }
        let retained_ids = retained_archive_indexes(core, &decisions);
        let new_cell_descendant = retained_ids
            .iter()
            .any(|id| core.archive.opened_new_cell(*id));
        let new_slot_descendant = retained_ids
            .iter()
            .any(|id| core.archive.opened_new_slot(*id));
        if !isolated_continuation {
            core.archive
                .record_selection_outcome(parent_index, !retained_ids.is_empty());
        }
        if isolated_continuation {
            let (landed, replaced) =
                continuation_arrival::<G>(&core.archive, pending_job.continuation_edge, &decisions);
            core.archive.record_continuation_outcome(
                landed,
                replaced,
                new_cell_descendant,
                pending_job.continuation_wave,
            );
            core.archive.add_continuation_execution_work(execution_work);
        }
        record_mixture_outcome(
            &mut core.mixture_energy,
            config.mixture,
            pending_job.selector.path,
            pending_job.mutation_seed,
            (pending_job.mixture_weight, pending_job.splice_weight),
            new_slot_descendant,
            execution_work,
        )?;
        if objectives_before == 0
            && let (Some(path), Some(input)) =
                (&config.objective_witness_path, &core.objective_witness)
        {
            std::fs::write(path, serde_json::to_vec_pretty(input)?)?;
        }
        let draw_checkpoint_after =
            finish_record(workload, &config.run, draw_state, core, &decisions)?;
        let draw_state_memory_bytes = workload.draw_state_memory_bytes(draw_state);
        if !memory_is_within_reserve(
            draw_state_memory_bytes,
            workload.draw_state_memory_reserve_bytes(&config.run),
        ) {
            return Err("live draw state exceeds its deterministic memory reserve".into());
        }
        if !memory_is_within_reserve(duration_policies.memory_bytes(), duration_memory_reserve) {
            return Err("live adaptive duration state exceeds its memory reserve".into());
        }
        core.archive.unpin_job_origin(pending_job.snapshot_id);
        core.archive.unpin_metadata(pending_job.parent_id);
        let compaction_started = profile_now();
        let compactions_before = core.archive.history_compactions();
        core.archive.maintain_memory_budget()?;
        core.archive.prepare_selection();
        record_compaction_elapsed(
            coordinator_profile,
            compactions_before,
            core.archive.history_compactions(),
            profile_elapsed(compaction_started),
        );
        coordinator_profile.bookkeeping_ns = coordinator_profile
            .bookkeeping_ns
            .saturating_add(profile_elapsed(bookkeeping_started));
        if let Some(admissions) = admissions.as_mut() {
            admissions.push(CampaignAdmission {
                reservation,
                decisions: decisions.clone(),
            });
        }
        let stream_started = profile_now();
        writer.write_line(&CampaignStreamRecord::Job(CampaignJobRecord {
            continuation_energy: pending_job.continuation_energy,
            sequence,
            reservation: u64::try_from(reservation)?,
            parent_id: pending_job.parent_id,
            mutation_seed: pending_job.mutation_seed,
            execution_work,
            result_sha256,
            decisions,
            mixture_weight: pending_job.mixture_weight,
            splice_weight: pending_job.splice_weight,
            splice: pending_job.splice,
            selector: pending_job.selector,
            draw_checkpoint_before: pending_job.draw_checkpoint_before,
            draw_checkpoint_after,
            duration_draw: pending_job.duration_draw,
            duration_admission_sequence_at_draw: pending_job.duration_admission_sequence_at_draw,
            duration_remaining_work: pending_job.duration_remaining_work,
            duration_checkpoint_at_draw: pending_job.duration_checkpoint_at_draw,
            duration_checkpoint_before,
            duration_checkpoint_after,
            suffix_limit: pending_job.suffix_limit,
        }))?;
        coordinator_profile.stream_write_ns = coordinator_profile
            .stream_write_ns
            .saturating_add(profile_elapsed(stream_started));
        counters.job_execution_work = counters.job_execution_work.saturating_add(execution_work);
        if objectives_before == 0 && core.objectives_reached > 0 {
            counters.note_first_objective(sequence);
        }
        if core.hang_streak() > 0 {
            match &core.hang_stop {
                Some(stop) if !stopped_before => {
                    eprintln!("{stop}; finishing the jobs in flight");
                }
                Some(_) => eprintln!(
                    "campaign reservation {reservation} hung; recorded as hung job {}",
                    core.hung_jobs()
                ),
                None => eprintln!(
                    "campaign reservation {reservation} hung; recorded as hung job {} and continuing",
                    core.hung_jobs()
                ),
            }
        }
        if let Some(sink) = progress.as_deref_mut()
            && progress_checkpoint_due(sequence)
        {
            let host_times = profile_printed
                .then(|| HostTimes::measure(*coordinator_schedstat, idle_clock.totals()));
            write_live_progress(
                core,
                counters,
                host_times.map(|host_times| (&*coordinator_profile, host_times)),
                draw_state_memory_bytes,
                *telemetry_started,
                false,
                sink,
            )?;
            if let Some(host_times) = host_times {
                eprintln!(
                    "coordinator-profile executions={sequence} coordinator_cpu_ns={} coordinator_run_wait_ns={} idle_admission_order_ns={} idle_no_job_ns={} receive_wait_ns={} admission_ns={} bookkeeping_ns={} history_compaction_ns={} stream_write_ns={} selection_ns={} receives={} admissions={} selections={} entries={} active_entries={} historical_input_actions={} stored_input_actions={} input_index_nodes={} resident_snapshots={} resident_snapshot_bytes={} entry_metadata_memory_bytes={} input_index_memory_bytes={} cell_memory_bytes={} history_memory_bytes={} draw_state_memory_bytes={} resident_memory_bytes={} snapshot_evictions={} history_compactions={} historical_entries_dropped={} input_reconstructions={} completed_buffered={} completed_actions_buffered={} completed_candidates_buffered={} completed_results_peak={} completed_actions_peak={} completed_candidates_peak={} job_execution_work={} replay_jobs={} replay_actions={} replay_cost={} suffix_actions={} suffix_cost={}",
                    host_times.coordinator_cpu_ns,
                    host_times.coordinator_run_wait_ns,
                    host_times.idle_admission_order_ns,
                    host_times.idle_no_job_ns,
                    coordinator_profile.receive_wait_ns,
                    coordinator_profile.admission_ns,
                    coordinator_profile.bookkeeping_ns,
                    coordinator_profile.history_compaction_ns,
                    coordinator_profile.stream_write_ns,
                    coordinator_profile.selection_ns,
                    coordinator_profile.receives,
                    coordinator_profile.admissions,
                    coordinator_profile.selections,
                    core.archive.entries.len(),
                    core.archive.active_count(),
                    core.archive.historical_input_actions(),
                    core.archive.stored_input_actions(),
                    core.archive.input_index_nodes(),
                    core.archive.resident_snapshot_count(),
                    core.archive.resident_snapshot_bytes(),
                    core.archive.entry_metadata_memory_bytes(),
                    core.archive.input_index_memory_bytes(),
                    core.archive.cell_memory_bytes(),
                    core.archive.history_memory_bytes(),
                    draw_state_memory_bytes,
                    core.archive
                        .resident_memory_bytes()
                        .saturating_add(draw_state_memory_bytes),
                    core.archive.snapshot_evictions(),
                    core.archive.history_compactions(),
                    core.archive.historical_entries_dropped(),
                    core.archive.input_reconstructions(),
                    completed.len(),
                    completed.telemetry.actions_live,
                    completed.telemetry.candidates_live,
                    completed.telemetry.results_peak,
                    completed.telemetry.actions_peak,
                    completed.telemetry.candidates_peak,
                    counters.job_execution_work,
                    coordinator_profile.replay_jobs,
                    coordinator_profile.replay_actions,
                    coordinator_profile.replay_cost,
                    coordinator_profile.suffix_actions,
                    coordinator_profile.suffix_cost,
                );
            }
        }
        *admitted = admitted.saturating_add(1);
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn select(&mut self) -> Result<Option<SelectedJob<G>>, Box<dyn Error>> {
        let workload = self.workload;
        let config = self.config;
        let work_budget = self.work_budget;
        let started = self.wall_started;
        let Self {
            core,
            rand,
            draw_state,
            duration_policies,
            writer,
            counters,
            reserved,
            ..
        } = self;
        workload.verify_integrity()?;
        if *reserved >= config.execution_budget
            || stop_reservations_after_work_budget(
                work_budget,
                counters.bootstrap_execution_work,
                counters.job_execution_work,
            )
            || stop_reservations_after_objective(
                config.stop_campaign_on_objective,
                core.objective_witness.is_some(),
            )
            || core.hang_stop.is_some()
        {
            return Ok(None);
        }
        if let (Some(started), Some(wall_budget)) = (started, config.wall_budget)
            && started.elapsed() >= wall_budget
        {
            return Ok(None);
        }
        let mut consecutive_skips = 0_u64;
        let (continuation_energy, continuation) =
            continuation_attempt(core, config.campaign_seed, *reserved);
        if let Some(continuation) = continuation {
            let parent_index = continuation.parent_index;
            let suffix = continuation.suffix;
            let (mixture_weight, splice_weight) = (0, u8::MAX);
            let mut mutation_seed = rand.next_u64();
            while energy_strategy(mutation_seed, mixture_weight, splice_weight)?
                != EnergyStrategy::Splice
            {
                mutation_seed = rand.next_u64();
            }
            let parent_id = continuation.parent_id;
            let selector = SelectorDraw {
                path: SelectorPath::Continuation,
                tier_rank: None,
                best_preference: None,
            };
            let draw_checkpoint_before = workload.draw_checkpoint(draw_state)?;
            let splice = Some(CampaignSpliceRecord::Tail {
                donor_id: continuation.donor,
                leaf_id: continuation.leaf,
                tail_postcard: postcard::to_allocvec(&suffix)?,
            });
            *reserved = reserved.saturating_add(1);
            core.archive.pin_metadata(parent_id)?;
            let (snapshot, replay, replay_certificates, snapshot_id) =
                core.archive.pin_job_origin(parent_index)?;
            let entry = &core.archive.entries[parent_index];
            return Ok(Some((
                JobSpec {
                    reservation: 0,
                    planned_finish: 0,
                    snapshot,
                    replay,
                    replay_certificates,
                    parent_milestones: entry.milestones,
                    suffix,
                },
                PendingJob {
                    snapshot_id,
                    parent_id,
                    continuation_energy,
                    continuation_wave: continuation.wave.saturating_add(1),
                    continuation_edge: Some((continuation.source, continuation.destination)),
                    mutation_seed,
                    mixture_weight,
                    splice_weight,
                    splice,
                    selector,
                    draw_checkpoint_before,
                    duration_draw: None,
                    duration_admission_sequence_at_draw: None,
                    duration_remaining_work: None,
                    duration_checkpoint_at_draw: None,
                    suffix_limit: None,
                    planned_finish: 0,
                },
            )));
        }
        loop {
            let (parent_index, selector) = core.archive.select_parent(rand)?;
            let parent_id = core
                .archive
                .stable_id(parent_index)
                .ok_or("selected archive slot is missing")?;
            let mutation_seed = rand.next_u64();
            let (mixture_weight, splice_weight) =
                mixture_weights(&core.mixture_energy, config.mixture);
            let draw_checkpoint_before = workload.draw_checkpoint(draw_state)?;
            let (spliced, splice) =
                if energy_strategy(mutation_seed, mixture_weight, splice_weight)?
                    == EnergyStrategy::Splice
                {
                    match core
                        .archive
                        .splice_tail_for_campaign(parent_index, LONGEST_SPLICE_TAIL)
                    {
                        Some(CampaignSpliceTail {
                            donor_id,
                            leaf_id,
                            actions,
                        }) => {
                            let tail_postcard = postcard::to_allocvec(&actions)?;
                            (
                                Some(actions),
                                Some(CampaignSpliceRecord::Tail {
                                    donor_id: core
                                        .archive
                                        .stable_id(donor_id)
                                        .ok_or("splice donor slot is missing")?,
                                    leaf_id: core
                                        .archive
                                        .stable_id(leaf_id)
                                        .ok_or("splice leaf slot is missing")?,
                                    tail_postcard,
                                }),
                            )
                        }
                        None => (None, Some(CampaignSpliceRecord::Unavailable)),
                    }
                } else {
                    (None, None)
                };
            let remaining_work = work_budget.and_then(|budget| {
                NonZeroU64::new(
                    budget.saturating_sub(
                        counters
                            .bootstrap_execution_work
                            .saturating_add(counters.job_execution_work),
                    ),
                )
            });
            let mut duration_checkpoint_at_draw = None;
            let lineage_duration = core.lineage_duration(workload, &config.run, parent_index);
            let duration_draw = if spliced.is_none() {
                workload
                    .duration_request(
                        &config.run,
                        core.archive.entries[parent_index].key,
                        remaining_work,
                    )
                    .map(|request| {
                        duration_checkpoint_at_draw =
                            duration_policies.context_checkpoint(request.context);
                        let mut duration_rand = RomuDuoJrRand::with_seed(mutation_seed);
                        duration_policies.draw(
                            request.context,
                            &mut duration_rand,
                            request.max_duration,
                            lineage_duration,
                        )
                    })
            } else {
                None
            };
            let duration_remaining_work = duration_draw.and(remaining_work);
            let duration_admission_sequence_at_draw = duration_draw.map(|_| core.sequence);
            let previous = core.archive.last_action(parent_index);
            let suffix_limit = spliced
                .is_none()
                .then(|| core.suffix_limit(parent_id, parent_index));
            let mut suffix = match (spliced, duration_draw) {
                (Some(tail), _) => tail,
                (None, Some(draw)) => workload.expand_suffix_duration(
                    &config.run,
                    draw_state,
                    MixtureDraw {
                        mixture: config.mixture,
                        weight: mixture_weight,
                        splice_weight,
                    },
                    mutation_seed,
                    previous,
                    draw,
                )?,
                (None, None) => workload.expand_suffix(
                    &config.run,
                    draw_state,
                    MixtureDraw {
                        mixture: config.mixture,
                        weight: mixture_weight,
                        splice_weight,
                    },
                    mutation_seed,
                    previous,
                )?,
            };
            if let Some(limit) = suffix_limit {
                suffix.truncate(usize::from(limit));
            }
            let all_prefixes_archived = consecutive_skips < CONSECUTIVE_SKIP_LIMIT
                && core.all_prefixes_archived(parent_index, &suffix);
            if all_prefixes_archived {
                let duration_checkpoint_before = duration_draw
                    .and_then(|draw| duration_policies.context_checkpoint(draw.context));
                let draw_checkpoint_after =
                    workload.finish_stream_record(&config.run, draw_state, &[], &core.evidence)?;
                writer.write_line(&CampaignStreamRecord::Skip(CampaignSkipRecord {
                    reservation: *reserved,
                    parent_id,
                    mutation_seed,
                    mixture_weight,
                    splice_weight,
                    splice,
                    selector,
                    draw_checkpoint_before,
                    draw_checkpoint_after,
                    duration_draw,
                    duration_admission_sequence_at_draw,
                    duration_remaining_work,
                    duration_checkpoint_at_draw,
                    duration_checkpoint_after: duration_checkpoint_before.clone(),
                    duration_checkpoint_before,
                    suffix_limit,
                }))?;
                core.archive.record_selection(parent_index, &selector);
                core.archive.maintain_memory_budget()?;
                core.archive.prepare_selection();
                counters.duplicates_skipped = counters.duplicates_skipped.saturating_add(1);
                consecutive_skips = consecutive_skips.saturating_add(1);
                continue;
            }
            *reserved = reserved.saturating_add(1);
            core.start_suffix_job(parent_id, suffix_limit);
            core.archive.pin_metadata(parent_id)?;
            let (snapshot, replay, replay_certificates, snapshot_id) =
                core.archive.pin_job_origin(parent_index)?;
            let entry = &core.archive.entries[parent_index];
            return Ok(Some((
                JobSpec {
                    reservation: 0,
                    planned_finish: 0,
                    snapshot,
                    replay,
                    replay_certificates,
                    parent_milestones: entry.milestones,
                    suffix,
                },
                PendingJob {
                    snapshot_id,
                    parent_id,
                    continuation_energy,
                    continuation_wave: 0,
                    continuation_edge: None,
                    mutation_seed,
                    mixture_weight,
                    splice_weight,
                    splice,
                    selector,
                    draw_checkpoint_before,
                    duration_draw,
                    duration_admission_sequence_at_draw,
                    duration_remaining_work,
                    duration_checkpoint_at_draw,
                    suffix_limit,
                    planned_finish: 0,
                },
            )));
        }
    }
}
