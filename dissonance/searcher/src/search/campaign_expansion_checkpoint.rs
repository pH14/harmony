// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::search::checkpoint::publisher::{AdmissionVersion, CheckpointPublisher};
use serde::ser::SerializeSeq;

const BODY_FORMAT: &str = "dissonance-bounded-expansion-checkpoint-body-v1";

pub(super) fn header<G: ExpansionWorkload>(
    workload: &G,
    config: &CampaignConfig<G>,
    executable: &str,
    reason: &str,
) -> CheckpointHeader {
    let mut policies = workload.policies(&config.run);
    for (name, value) in [
        ("algorithm", BODY_FORMAT.to_owned()),
        ("schedule_policy", EXPANSION_SCHEDULE.to_owned()),
        ("expansion_policy", workload.expansion_policy().to_owned()),
        ("parent_scheduler", SELECTOR_IDENTIFIER.to_owned()),
        ("mixture_policy", draw_mixture_identifier(config.mixture)),
        (
            "retention_policy",
            retention_policy_identifier(config.retention).to_owned(),
        ),
        (
            "stop_rollout_on_objective",
            config.stop_rollout_on_objective.to_string(),
        ),
        (
            "stop_campaign_on_objective",
            config.stop_campaign_on_objective.to_string(),
        ),
        (
            "selection_seed_derivation",
            SELECTION_SEED_DERIVATION.to_owned(),
        ),
        ("local_width", expansion::WIDTH.to_string()),
        ("local_fanout", expansion::FANOUT.to_string()),
        ("local_depth", expansion::DEPTH.to_string()),
        (
            "cost_policy",
            "root_relative_actual_action_cost_v1".to_owned(),
        ),
        (
            "budget_policy",
            "edge_attempts_empty_task_charge_one_v1".to_owned(),
        ),
    ] {
        policies.insert(name.to_owned(), value);
    }
    CheckpointHeader {
        format: SEARCH_CHECKPOINT_FORMAT.to_owned(),
        checkpoint_owner: None,
        reason: reason.to_owned(),
        workload_identity_sha256: workload.workload_identity_sha256(),
        executable_identity: executable.to_owned(),
        campaign_seed: config.campaign_seed,
        window: config.window,
        archive_entry_limit: config.archive_entry_limit,
        memory_budget_mib: config.memory_budget_mib,
        policies,
        executions: 0,
        reserved: 0,
        admitted: 0,
        planned_clock: 0,
    }
}

#[derive(Serialize)]
#[serde(bound = "")]
struct ReservationRef<'a, G: ExpansionWorkload> {
    task: u64,
    parent_id: u64,
    snapshot_id: u64,
    replay: &'a [G::Action],
    replay_certificates: &'a [Option<[u8; 32]>],
    parent_key: G::Key,
    parent_milestones: G::Milestones,
    root_cost_in_group: u64,
    last_action: Option<G::Action>,
    task_seed: u64,
    edge_budget: NonZeroU8,
}
impl<'a, G: ExpansionWorkload> From<&'a ExpansionTaskSpec<G>> for ReservationRef<'a, G> {
    fn from(spec: &'a ExpansionTaskSpec<G>) -> Self {
        Self {
            task: spec.task,
            parent_id: spec.parent_id,
            snapshot_id: spec.snapshot_id,
            replay: &spec.replay,
            replay_certificates: &spec.replay_certificates,
            parent_key: spec.parent_key,
            parent_milestones: spec.parent_milestones,
            root_cost_in_group: spec.root_cost_in_group,
            last_action: spec.last_action,
            task_seed: spec.task_seed,
            edge_budget: spec.edge_budget,
        }
    }
}

struct Reservations<'a, 'g, G: ExpansionWorkload, N>(&'a ExpansionCoordinator<'g, G, N>);
impl<G: ExpansionWorkload, N> Serialize for Reservations<'_, '_, G, N> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let campaign = self.0;
        if campaign.fifo.len() != campaign.reservations.len() {
            return Err(serde::ser::Error::custom(
                "expansion checkpoint reservation index differs from FIFO",
            ));
        }
        let mut seq = serializer.serialize_seq(Some(campaign.fifo.len()))?;
        for (offset, token) in campaign.fifo.iter().copied().enumerate() {
            let slot = campaign.reservations.slot(token).ok_or_else(|| {
                serde::ser::Error::custom("expansion checkpoint has a stale FIFO token")
            })?;
            let expected = campaign
                .counters
                .tasks_admitted
                .checked_add(offset as u64)
                .ok_or_else(|| {
                    serde::ser::Error::custom("expansion checkpoint task identity overflow")
                })?;
            if slot.admitting || slot.key != expected || slot.pending.spec.task != expected {
                return Err(serde::ser::Error::custom(
                    "expansion checkpoint is not at a canonical task boundary",
                ));
            }
            seq.serialize_element(&ReservationRef::from(&slot.pending.spec))?;
        }
        seq.end()
    }
}

pub(super) fn publish<G: ExpansionWorkload, N>(
    campaign: &ExpansionCoordinator<'_, G, N>,
    publisher: &mut CheckpointPublisher<G::Snapshot, <G::Key as ArchiveKey>::Progress>,
    reason: &str,
) -> Result<(), Box<dyn Error>> {
    campaign.workload.verify_integrity()?;
    publisher.publish(
        AdmissionVersion {
            executions: campaign.core.sequence,
            reserved: campaign.counters.tasks_reserved,
            admitted: usize::try_from(campaign.counters.tasks_admitted)?,
            planned_clock: 0,
        },
        reason,
        campaign.core.archive.resident_snapshot_entries(),
        |out| {
            postcard::to_io(&BODY_FORMAT, &mut *out)?;
            campaign.core.write_checkpoint_state(&mut *out)?;
            postcard::to_io(&campaign.core.archive, &mut *out)?;
            crate::search::checkpoint::image::write_streamed_bytes(&mut *out, |out| {
                G::write_evidence_checkpoint(&campaign.core.evidence, out)
            })?;
            postcard::to_io(&campaign.rand, &mut *out)?;
            postcard::to_io(&campaign.counters, &mut *out)?;
            postcard::to_io(&Reservations(campaign), &mut *out)?;
            Ok(())
        },
    )
}

fn validate_counters<G: ExpansionWorkload>(
    state: &CoreCheckpoint<G>,
    counts: &ExpansionCounters,
    header: &CheckpointHeader,
    records: &[ExpansionReservationRecord<G>],
) -> Result<(), Box<dyn Error>> {
    let in_flight = counts
        .tasks_reserved
        .checked_sub(counts.tasks_admitted)
        .ok_or("expansion checkpoint admitted more tasks than it reserved")?;
    let edge_failures = counts
        .edge_attempts
        .checked_sub(counts.executed_edges)
        .ok_or("expansion checkpoint executed edge count exceeds attempts")?;
    let root_failures = counts
        .preparation_failures
        .checked_sub(edge_failures)
        .ok_or("expansion checkpoint preparation failure count is inconsistent")?;
    if header.executions != state.sequence
        || counts.edge_attempts != state.sequence
        || header.reserved != counts.tasks_reserved
        || u64::try_from(header.admitted)? != counts.tasks_admitted
        || header.planned_clock != 0
        || in_flight != u64::try_from(records.len())?
        || records.len() > header.window
        || counts.empty_tasks > counts.tasks_admitted
        || root_failures > counts.empty_tasks
        || counts.edge_attempts.checked_add(counts.empty_tasks) != Some(counts.budget_consumed)
        || counts.tasks_admitted > counts.budget_consumed
        || u128::from(counts.budget_consumed)
            > u128::from(counts.tasks_admitted) * expansion::MAX_EDGES as u128
    {
        return Err("expansion checkpoint counters disagree with its task boundary".into());
    }
    let total_work = u128::from(counts.bootstrap_work)
        + u128::from(counts.reconstruction_work)
        + u128::from(counts.execution_work);
    let objective = state.objectives_reached > 0;
    if state.objective_witness.is_some() != objective
        || counts.executions_to_first_objective.is_some() != objective
        || counts.work_to_first_objective.is_some() != objective
        || u128::from(state.objectives_reached)
            > u128::from(counts.executed_edges)
                + u128::from(counts.executions_to_first_objective == Some(0))
        || counts
            .executions_to_first_objective
            .is_some_and(|first| first > counts.executed_edges)
        || counts.work_to_first_objective.is_some_and(|first| {
            first < counts.bootstrap_work
                || u128::from(first) > total_work
                || counts.executions_to_first_objective == Some(0) && first != counts.bootstrap_work
        })
    {
        return Err("expansion checkpoint first-objective counters are inconsistent".into());
    }
    Ok(())
}

pub(super) fn restore<'a, G: ExpansionWorkload, N>(
    workload: &'a G,
    config: &'a CampaignConfig<G>,
    mut reader: CheckpointReader,
) -> Result<ExpansionCoordinator<'a, G, N>, Box<dyn Error>> {
    workload.verify_integrity()?;
    if config.workers == 0
        || config.window == 0
        || !archive_entry_limit_is_valid(config.archive_entry_limit)
        || config.memory_budget_mib == Some(0)
        || config.mixture != DrawMixture::AlphabetOnly
        || config.retention != RetentionPolicy::Unprobed
        || !config.stop_rollout_on_objective
    {
        return Err("expansion checkpoint resume configuration is invalid".into());
    }
    let stored = reader.header.clone();
    let executable = workload.executable_identity()?;
    let expected = header(workload, config, &executable, &stored.reason);
    if stored.format != expected.format
        || stored.executable_identity != expected.executable_identity
        || stored.workload_identity_sha256 != expected.workload_identity_sha256
        || stored.campaign_seed != expected.campaign_seed
        || stored.window != expected.window
        || stored.archive_entry_limit != expected.archive_entry_limit
        || stored.memory_budget_mib != expected.memory_budget_mib
        || stored.policies != expected.policies
    {
        return Err(
            "expansion checkpoint source, policy or bounds differ from the requested campaign"
                .into(),
        );
    }
    let format: String = reader.next()?;
    if format != BODY_FORMAT {
        return Err("expansion checkpoint body algorithm is not recognized".into());
    }
    let state: CoreCheckpoint<G> = reader.next()?;
    let mut archive: Archive<G::Action, G::Key, G::Milestones, G::Snapshot> = reader.next()?;
    let evidence: Vec<u8> = reader.next()?;
    let rand: RomuDuoJrRand = reader.next()?;
    let counters: ExpansionCounters = reader.next()?;
    let records: Vec<ExpansionReservationRecord<G>> = reader.next()?;
    validate_counters(&state, &counters, &stored, &records)?;
    let snapshots = reader.snapshots::<G::Snapshot>()?;
    reader.finish()?;
    let mut snapshot_ids = BTreeSet::new();
    if snapshots.iter().any(|(id, _)| !snapshot_ids.insert(*id)) {
        return Err("expansion checkpoint has duplicate snapshot IDs".into());
    }
    let limit = config.memory_budget_mib.map(|mib| {
        let reserve = workload
            .draw_state_memory_reserve_bytes(&config.run)
            .saturating_add(DurationPolicies::<G::Key>::memory_reserve_bytes());
        mib.saturating_mul(1024 * 1024).saturating_sub(reserve)
    });
    if archive.max_entries != config.archive_entry_limit || archive.memory_budget_bytes() != limit {
        return Err("expansion checkpoint archive limits differ from its header".into());
    }
    archive.set_snapshot_retention(G::snapshot_retention());
    archive.restore_runtime(
        workload.action_cost_fn(),
        Some(G::snapshot_memory_charge),
        snapshots,
    )?;
    let mut metadata = BTreeMap::new();
    let mut snapshot_pins = BTreeMap::new();
    let mut claims = 0u64;
    let mut specs = Vec::new();
    specs.try_reserve_exact(records.len())?;
    for (offset, record) in records.into_iter().enumerate() {
        if record.task
            != counters
                .tasks_admitted
                .checked_add(u64::try_from(offset)?)
                .ok_or("expansion task identity overflow")?
            || record.task >= counters.tasks_reserved
            || usize::from(record.edge_budget.get()) > expansion::MAX_EDGES
        {
            return Err("expansion checkpoint task order or quota is invalid".into());
        }
        claims = claims
            .checked_add(u64::from(record.edge_budget.get()))
            .ok_or("expansion checkpoint claims overflow")?;
        let index = archive
            .index_of_id(record.parent_id)
            .ok_or("expansion checkpoint root ID is missing")?;
        let parent = &archive.entries[index];
        if parent.key != record.parent_key
            || parent.milestones != record.parent_milestones
            || archive.entry_cost_in_group(index) != record.root_cost_in_group
            || archive.last_action(index).copied() != record.last_action
        {
            return Err("expansion checkpoint root metadata differs from the archive".into());
        }
        let snapshot_index = archive
            .index_of_id(record.snapshot_id)
            .ok_or("expansion checkpoint snapshot origin ID is missing")?;
        let recorded_snapshot = archive.entries[snapshot_index]
            .snapshot
            .as_ref()
            .ok_or("expansion checkpoint snapshot origin has no snapshot")?;
        let (snapshot, replay, certificates) =
            archive.job_origin_from(index, record.snapshot_id)?;
        if !Arc::ptr_eq(&snapshot, recorded_snapshot)
            || replay != record.replay
            || certificates != record.replay_certificates
        {
            return Err("expansion checkpoint replay origin differs from the archive".into());
        }
        let pins = metadata.entry(record.parent_id).or_insert(0u32);
        *pins = pins
            .checked_add(1)
            .ok_or("expansion checkpoint metadata pin overflow")?;
        let pins = snapshot_pins.entry(record.snapshot_id).or_insert(0u32);
        *pins = pins
            .checked_add(1)
            .ok_or("expansion checkpoint snapshot pin overflow")?;
        specs.push(ExpansionTaskSpec {
            task: record.task,
            parent_id: record.parent_id,
            snapshot_id: record.snapshot_id,
            snapshot,
            replay: record.replay,
            replay_certificates: record.replay_certificates,
            parent_key: record.parent_key,
            parent_milestones: record.parent_milestones,
            root_cost_in_group: record.root_cost_in_group,
            last_action: record.last_action,
            task_seed: record.task_seed,
            edge_budget: record.edge_budget,
        });
    }
    archive.validate_inflight_pins(&metadata, &snapshot_pins)?;
    if counters
        .budget_consumed
        .checked_add(claims)
        .is_none_or(|used| used > config.execution_budget)
    {
        return Err("expansion checkpoint budget does not cover consumed and reserved work".into());
    }
    if !resident_memory_is_within_budget(archive.resident_memory_bytes(), config.memory_budget_mib)
    {
        return Err("expansion checkpoint restored archive exceeds its memory budget".into());
    }
    let mut core = CoordinatorCore::new(
        workload,
        &config.run,
        config.archive_entry_limit,
        config.memory_budget_mib,
    );
    core.restore_state(state)?;
    core.archive = archive;
    core.evidence = G::evidence_from_checkpoint(&evidence)?;
    core.archive.prepare_selection();
    let capacity = config
        .window
        .min(usize::try_from(config.execution_budget).unwrap_or(usize::MAX));
    let mut reservations = ReservationSlots::try_new(expansion_records::<G, N>, capacity)?;
    let mut fifo = VecDeque::new();
    fifo.try_reserve_exact(capacity)?;
    for spec in specs {
        let task = spec.task;
        if reservations
            .insert(task, PendingExpansion { spec })
            .is_some()
        {
            return Err("expansion checkpoint repeats a reservation".into());
        }
        fifo.push_back(
            reservations
                .token(&task)
                .ok_or("expansion checkpoint lost a restored reservation")?,
        );
    }
    workload.verify_integrity()?;
    Ok(ExpansionCoordinator {
        workload,
        config,
        core,
        rand,
        counters,
        claims,
        reservations,
        fifo,
        failed: false,
        stopped: false,
        checkpoints: None,
    })
}
