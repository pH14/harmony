// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use std::io::BufRead;

const MAX_RECORD_BYTES: u64 = 256 * 1024 * 1024;

fn read_record(
    input: &mut impl BufRead,
    hasher: &mut Sha256,
) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    input
        .take(MAX_RECORD_BYTES + 1)
        .read_until(b'\n', &mut bytes)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() as u64 > MAX_RECORD_BYTES || bytes.last() != Some(&b'\n') {
        return Err("expansion stream record exceeds its byte bound or is truncated".into());
    }
    hasher.update(&bytes);
    Ok(Some(bytes))
}

pub fn replay_expansion<G: ExpansionWorkload>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOrigin<G>,
    input: &mut dyn Read,
    expected: &ExpansionRunReport<G::Action, G::ArchiveReport>,
) -> Result<ExpansionOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
{
    workload.verify_integrity()?;
    let mut reader = BufReader::new(input);
    let mut hasher = Sha256::new();
    let first = read_record(&mut reader, &mut hasher)?.ok_or("expansion stream has no header")?;
    let header: ExpansionStreamHeader = serde_json::from_slice(&first)?;
    let executable = workload.executable_identity()?;
    let (origin_record, pinned_origin) = expansion_origin(workload, config, origin, &executable)?;
    let origin = pinned_origin.as_ref().unwrap_or(origin);
    let current_header = ExpansionStreamHeader::new(
        workload,
        config,
        origin_record,
        executable,
        expected.header.work_budget,
    );
    if serde_json::to_vec(&header)? != serde_json::to_vec(&current_header)?
        || serde_json::to_vec(&header)? != serde_json::to_vec(&expected.header)?
    {
        return Err("expansion replay header differs from the requested source or policy".into());
    }
    let mut campaign = ExpansionCoordinator::<G>::start(workload, config, origin)?;
    let initial_counters = campaign.counters;
    let mut target = workload.new_target()?;
    if header.work_budget == Some(0) {
        return Err("expansion work budget must be nonzero".into());
    }
    let (final_counters, stop_reason) = loop {
        let bytes =
            read_record(&mut reader, &mut hasher)?.ok_or("expansion stream has no final record")?;
        let record: ExpansionStreamRecord<G> = serde_json::from_slice(&bytes)?;
        match record {
            ExpansionStreamRecord::Task(record) => {
                if record.reserved_through < campaign.counters.tasks_reserved
                    || record.reserved_through > config.execution_budget
                {
                    return Err("expansion replay reservation watermark is invalid".into());
                }
                while campaign.counters.tasks_reserved < record.reserved_through {
                    campaign
                        .reserve()?
                        .ok_or("expansion replay cannot reserve the recorded task window")?;
                }
                let token = *campaign
                    .fifo
                    .front()
                    .ok_or("expansion replay task has no reservation")?;
                let pending = &campaign
                    .reservations
                    .slot(token)
                    .ok_or("expansion replay lost its pending task")?
                    .pending;
                if serde_json::to_vec(&ExpansionReservationRecord::from(&pending.spec))?
                    != serde_json::to_vec(&record.reservation)?
                {
                    return Err("expansion replay selected a different root or task seed".into());
                }
                let spec = pending.spec.clone();
                let outcome = run_expansion_task(workload, &config.run, &mut target, &spec, &())?;
                campaign.complete(outcome)?;
                let mut admission = campaign
                    .admit_next()?
                    .ok_or("expansion replay task was not ready for admission")?;
                let actual =
                    ExpansionTaskRecord::take(&mut admission, campaign.counters.tasks_reserved);
                if serde_json::to_vec(&actual)? != serde_json::to_vec(&record)? {
                    return Err(format!("expansion task {} replay differs in its tree, evidence, frontier or admissions", spec.task).into());
                }
            }
            ExpansionStreamRecord::End {
                counters,
                stop_reason,
            } => break (counters, stop_reason),
        }
    };
    if !campaign.finished()
        || final_counters != campaign.counters
        || final_counters != expected.counters
    {
        return Err("expansion replay ended with a task gap or different counters".into());
    }
    let sufficient = match stop_reason {
        ExpansionStopReason::Budget => final_counters.budget_consumed >= config.execution_budget,
        ExpansionStopReason::WorkBudget => header.work_budget.is_some_and(|limit| {
            u128::from(final_counters.bootstrap_work)
                + u128::from(final_counters.reconstruction_work)
                + u128::from(final_counters.execution_work)
                >= u128::from(limit)
        }),
        ExpansionStopReason::Objective => {
            config.stop_campaign_on_objective && campaign.objective_witness().is_some()
        }
        ExpansionStopReason::WallBudget => header.wall_budget_seconds.is_some(),
    };
    if !sufficient || stop_reason != expected.stop_reason {
        return Err("expansion replay final record has no sufficient stopping condition".into());
    }
    if read_record(&mut reader, &mut hasher)?.is_some() {
        return Err("expansion stream continues after its final record".into());
    }
    let stream_sha256 = format!("{:x}", hasher.finalize());
    if stream_sha256 != expected.stream_sha256 {
        return Err("expansion stream differs from its reported digest".into());
    }
    workload.verify_integrity()?;
    campaign.core.archive.compact_history_for_final_report()?;
    campaign.core.finish_curve();
    let objective_witness = campaign.core.objective_witness.clone();
    let counters = campaign.counters;
    let (archive, snapshots) = campaign.core.into_archive_report_and_snapshots(
        workload,
        config.campaign_seed,
        config.materialize_final_artifacts,
    )?;
    let report = ExpansionRunReport {
        header,
        counters,
        initial_counters,
        stop_reason,
        objective_witness,
        stream_sha256,
        archive,
        telemetry: CampaignTelemetry::default(),
    };
    if serde_json::to_vec(&report)? != serde_json::to_vec(expected)? {
        return Err("expansion replay logical report differs from its recorded report".into());
    }
    Ok((
        report,
        SnapshotCheckpoint {
            format: workload.checkpoint_format().into(),
            entries: snapshots
                .into_iter()
                .map(|(id, snapshot)| SnapshotCheckpointEntry { id, snapshot })
                .collect(),
        },
    ))
}
