// SPDX-License-Identifier: AGPL-3.0-or-later

use super::profile::{ExpansionCpuProfile, ReceiveStatus, profile_cpu_now};
use super::*;
use crate::search::telemetry::{
    CoordinatorPhaseTelemetry, IdleClock, IdleReason, nanos_since, thread_schedstat,
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound(
    serialize = "A: Ord + Serialize, R: Serialize",
    deserialize = "A: Ord + DeserializeOwned, R: DeserializeOwned"
))]
pub struct ExpansionRunReport<A: Ord, R> {
    pub header: ExpansionStreamHeader,
    pub counters: ExpansionCounters,
    pub initial_counters: ExpansionCounters,
    pub stop_reason: ExpansionStopReason,
    pub objective_witness: Option<Input<A>>,
    pub stream_sha256: String,
    pub archive: R,
    #[serde(skip)]
    pub telemetry: CampaignTelemetry,
}

pub type ExpansionOutcome<G> = (
    ExpansionRunReport<<G as CampaignTypes>::Action, <G as CampaignTypes>::ArchiveReport>,
    SnapshotCheckpoint<<G as CampaignTypes>::Snapshot>,
);

pub fn run_expansion_watched<G: ExpansionWorkload + 'static, W: ExpansionWatch<G>>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOrigin<G>,
    stream: &mut dyn Write,
    mut progress: Option<&mut dyn Write>,
    options: CampaignExecutionOptions,
    watch: &W,
) -> Result<ExpansionOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
    G::Action: 'static,
    G::Milestones: 'static,
    G::Observations: 'static,
    W::Note: 'static,
    <G::Key as ArchiveKey>::Progress: Send + Sync + 'static,
    G::ExpansionQuality: 'static,
    G::ExpansionBucket: 'static,
{
    if options.work_budget == Some(0) {
        return Err("expansion work budget must be nonzero".into());
    }
    workload.verify_integrity()?;
    let executable = workload.executable_identity()?;
    let (origin_record, pinned_origin) = expansion_origin(workload, config, origin, &executable)?;
    let origin = pinned_origin.as_ref().unwrap_or(origin);
    let header = ExpansionStreamHeader::new(
        workload,
        config,
        origin_record,
        executable,
        options.work_budget,
    );
    let mut writer: StreamWriter<'_, ExpansionStreamRecord<G>, ExpansionStreamCleanup<G, W::Note>> =
        StreamWriter::new_with_cleanup(stream);
    writer.write_header(&header)?;
    let placement = options.placement.as_ref();
    let checkpoint_plan = options.checkpoints.clone();
    let (bootstrapped, first_start) = mpsc::channel::<()>();
    let first_start = Mutex::new(first_start);
    let mut coordinator = None;
    let mut initial_counters = None;
    let mut telemetry = CampaignTelemetry::default();
    let cpu_profile_enabled = profile_clock_enabled();
    let mut cpu_profile = ExpansionCpuProfile::new(cpu_profile_enabled);
    let search_started = now();
    let result = with_worker_pool_routed(
        config.workers,
        |worker| {
            if worker == 0 {
                first_start
                    .lock()
                    .map_err(|_| "expansion start lock poisoned".to_owned())?
                    .recv()
                    .map_err(|_| "expansion bootstrap did not complete".to_owned())?;
            }
            ThreadPlacement::place(placement, PlacedThread::Worker(worker))?;
            workload.new_target()
        },
        |_| workload.new_target(),
        |target, spec: ExpansionTaskSpec<G>| {
            run_expansion_task(workload, &config.run, target, &spec, watch)
                .map_err(|error| error.to_string())
        },
        |target| workload.telemetry(target),
        |target| workload.worker_locality(target),
        || workload.begin_job_observation(),
        || workload.take_job_observation(),
        None,
        |pool| -> Result<(), Box<dyn Error>> {
            ThreadPlacement::place(placement, PlacedThread::Coordinator)?;
            let bootstrap_started = now();
            let campaign =
                coordinator.insert(ExpansionCoordinator::start(workload, config, origin)?);
            initial_counters = Some(campaign.counters);
            if let Some(plan) = checkpoint_plan {
                campaign.enable_checkpoints(plan)?;
            }
            telemetry.bootstrap_ns = nanos_since(bootstrap_started);
            pool.configure_indexed(
                campaign.reservations.slots.len(),
                |key: &(u64, ReservationToken)| key.1.slot,
            )?;
            writer.enable_background(options.placement.clone())?;
            watch.start(campaign)?;
            bootstrapped.send(())?;
            drop(bootstrapped);
            pool.wait_ready(config.workers)?;
            let phase_started = now();
            let cpu_started = thread_schedstat();
            let dispatch_cpu_started = profile_cpu_now();
            let mut idle = IdleClock::new(config.workers as usize);
            let mut running = 0;
            let dispatch = |campaign: &mut ExpansionCoordinator<'_, G, W::Note>,
                            spec: ExpansionTaskSpec<G>|
             -> Result<bool, Box<dyn Error>> {
                let started = profile_now();
                let result = (|| {
                    let task = spec.task;
                    let token = campaign
                        .reservations
                        .token(&task)
                        .ok_or("expansion dispatch lost reservation")?;
                    let locality = workload.snapshot_locality(&spec.snapshot);
                    match pool.dispatch_local((task, token), spec, locality) {
                        Ok(()) => Ok(true),
                        Err(error) => {
                            campaign.complete_error(task, error.to_string())?;
                            campaign.stop_reservations();
                            Ok(false)
                        }
                    }
                })();
                record_profile(&mut campaign.profile.dispatch_ns, started);
                result
            };
            let reserve = |campaign: &mut ExpansionCoordinator<'_, G, W::Note>| -> Result<usize, Box<dyn Error>> {
                let mut dispatched = 0;
                if config.wall_budget.is_some_and(|limit| search_started.elapsed() >= limit)
                    || options.work_budget.is_some_and(|limit| {
                        u128::from(campaign.counters.bootstrap_work)
                            + u128::from(campaign.counters.reconstruction_work)
                            + u128::from(campaign.counters.execution_work) >= u128::from(limit)
                    })
                { campaign.stop_reservations(); }
                while let Some(spec) = campaign.reserve()? {
                    if !dispatch(campaign, spec)? { break; }
                    dispatched += 1;
                }
                Ok(dispatched)
            };
            for spec in campaign.pending_tasks()? {
                if !dispatch(campaign, spec)? {
                    break;
                }
                running += 1;
            }
            running += reserve(campaign)?;
            loop {
                while let Some(mut admission) = campaign.admit_next()? {
                    let started = profile_now();
                    watch.admitted(&mut admission, campaign);
                    record_profile(&mut campaign.profile.watch_ns, started);
                    let started = profile_now();
                    let record =
                        ExpansionTaskRecord::take(&mut admission, campaign.counters.tasks_reserved);
                    record_profile(&mut campaign.profile.record_ns, started);
                    let started = profile_now();
                    let cleanup = campaign.stream_cleanup(admission);
                    writer.write_line_with_cleanup(ExpansionStreamRecord::Task(record), cleanup)?;
                    record_profile(&mut campaign.profile.stream_ns, started);
                    let started = profile_now();
                    if let Some(progress) = progress.as_mut() {
                        serde_json::to_writer(&mut **progress, &campaign.counters)?;
                        progress.write_all(b"\n")?;
                    }
                    record_profile(&mut campaign.profile.progress_ns, started);
                    running += reserve(campaign)?;
                    let started = profile_now();
                    campaign.checkpoint_due()?;
                    record_profile(&mut campaign.profile.checkpoint_ns, started);
                }
                if campaign.finished() {
                    break;
                }
                let started = profile_now();
                let drained = writer.drain_ready();
                record_profile(&mut campaign.profile.drain_ns, started);
                drained?;
                let started = profile_now();
                let polled = campaign.poll_persistence();
                record_profile(&mut campaign.profile.persistence_poll_ns, started);
                polled?;
                idle.update(
                    running,
                    if campaign.reservations.completed_len() > 0 {
                        IdleReason::AdmissionOrder
                    } else {
                        IdleReason::NoJob
                    },
                );
                let receive_cpu_started = profile_cpu_now();
                let before = now();
                let received = pool.receive_timeout(Duration::from_millis(5));
                let waited = nanos_since(before);
                let receive_cpu_finished = profile_cpu_now();
                if cpu_profile_enabled {
                    cpu_profile.record_receive(
                        receive_cpu_started,
                        receive_cpu_finished,
                        match &received {
                            Ok(Some(_)) => ReceiveStatus::Reply,
                            Ok(None) => ReceiveStatus::Timeout,
                            Err(_) => ReceiveStatus::Error,
                        },
                    );
                }
                if campaign.reservations.completed_len() > 0 {
                    telemetry.admission.receive_head_of_line_ns = telemetry
                        .admission
                        .receive_head_of_line_ns
                        .saturating_add(waited);
                } else {
                    telemetry.admission.receive_all_running_ns = telemetry
                        .admission
                        .receive_all_running_ns
                        .saturating_add(waited);
                }
                let Some(reply) = received? else {
                    continue;
                };
                let (task, token) = reply.key;
                if reply.worker >= config.workers {
                    return Err("expansion worker identity is invalid".into());
                }
                campaign.reservations.validate_pending(token, task)?;
                running = running
                    .checked_sub(1)
                    .ok_or("expansion running task count underflow")?;
                match reply.outcome {
                    Ok(outcome) if outcome.task == task => campaign.complete(outcome)?,
                    Ok(_) => return Err("expansion worker returned another task".into()),
                    Err(error) => campaign.complete_error(task, error)?,
                }
            }
            idle.update(0, IdleReason::NoJob);
            let dispatch_cpu_finished = profile_cpu_now();
            let phase_wall_ns = nanos_since(phase_started);
            let phase_host = HostTimes::try_measure(cpu_started, idle.totals());
            cpu_profile.record_dispatch(dispatch_cpu_started, dispatch_cpu_finished);
            telemetry.dispatch_phase = Some(CoordinatorPhaseTelemetry {
                wall_ns: phase_wall_ns,
                host: phase_host,
            });
            idle.finish(&mut telemetry.admission);
            telemetry.completed_buffer = campaign.reservations.telemetry;
            pool.close();
            Ok(())
        },
    );
    telemetry.search_ns = nanos_since(search_started);
    match result {
        Ok(((), workers)) => telemetry.workers = workers,
        Err(error) => {
            let primary = match coordinator.as_mut() {
                Some(campaign) => campaign.finish_persistence(Err(error)),
                None => Err(error),
            };
            let drained = writer.finish().map(|_| ());
            return coordinator_session_result(primary, drained, &mut std::io::stderr());
        }
    }
    let mut campaign = coordinator.ok_or("expansion coordinator did not start")?;
    let persist_started = now();
    let published = campaign.checkpoint("final");
    campaign.finish_persistence(published)?;
    workload.verify_integrity()?;
    let stop_reason = if campaign.counters.budget_consumed >= config.execution_budget {
        ExpansionStopReason::Budget
    } else if config.stop_campaign_on_objective && campaign.objective_witness().is_some() {
        ExpansionStopReason::Objective
    } else if options.work_budget.is_some_and(|limit| {
        u128::from(campaign.counters.bootstrap_work)
            + u128::from(campaign.counters.reconstruction_work)
            + u128::from(campaign.counters.execution_work)
            >= u128::from(limit)
    }) {
        ExpansionStopReason::WorkBudget
    } else if config
        .wall_budget
        .is_some_and(|limit| search_started.elapsed() >= limit)
    {
        ExpansionStopReason::WallBudget
    } else {
        return Err("expansion task window drained before a stopping condition".into());
    };
    writer.write_line(ExpansionStreamRecord::End {
        counters: campaign.counters,
        stop_reason,
    })?;
    let finish_cpu_started = profile_clock_enabled().then(thread_schedstat).flatten();
    let started = profile_now();
    let finished = writer.finish();
    record_profile(&mut campaign.profile.stream_finish_ns, started);
    let stream_finish_host =
        finish_cpu_started.and_then(|before| HostTimes::try_measure(Some(before), (0, 0)));
    let (stream_sha256, stream_telemetry) = finished?;
    telemetry.stream = stream_telemetry;
    campaign.core.archive.compact_history_for_final_report()?;
    campaign.core.finish_curve();
    let objective_witness = campaign.core.objective_witness.clone();
    let counters = campaign.counters;
    let (archive, snapshots) = campaign.core.into_archive_report_and_snapshots(
        workload,
        config.campaign_seed,
        config.materialize_final_artifacts,
    )?;
    telemetry.persistence_ns = nanos_since(persist_started);
    if profile_clock_enabled() {
        telemetry.coordinator = serde_json::json!({
            "kind": "bounded_expansion_spans_v1",
            "basis": "inclusive_wall_ns_not_cpu",
            "spans": campaign.profile,
            "stream_finish_host": stream_finish_host,
            "cpu": {
                "clock": "thread_cpu_time",
                "scope": "profile_enabled_linux_current_coordinator_thread_dispatch_and_receive",
                "supported": cfg!(target_os = "linux"),
                "receive_cpu_complete": !cpu_profile.counter_overflow
                    && cpu_profile.receive_cpu_ns.is_some()
                    && cpu_profile.receive_cpu_samples == cpu_profile.receive_calls,
                "measurements": cpu_profile,
            },
        });
    }
    telemetry.sum_targets();
    let report = ExpansionRunReport {
        header,
        counters,
        initial_counters: initial_counters
            .ok_or("expansion dispatch counters were not initialized")?,
        stop_reason,
        objective_witness,
        stream_sha256,
        archive,
        telemetry,
    };
    let checkpoint = SnapshotCheckpoint {
        format: workload.checkpoint_format().into(),
        entries: snapshots
            .into_iter()
            .map(|(id, snapshot)| SnapshotCheckpointEntry { id, snapshot })
            .collect(),
    };
    Ok((report, checkpoint))
}
