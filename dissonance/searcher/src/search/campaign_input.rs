// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{
    CampaignConfig, Workload,
    coordinator::{JobOutcome, JobSpec, run_job},
};
use crate::search::{
    archive::Archive, draw::MixtureDraw, draw_tables::DrawTables, rand::RomuDuoJrRand,
};
use serde::Serialize;
use std::{error::Error, num::NonZeroU64};

pub(crate) struct InputExecutionContext<'a, G: Workload> {
    pub workload: &'a G,
    pub config: &'a CampaignConfig<G>,
    pub archive: &'a Archive<G::Action, G::Key, G::Milestones, G::Snapshot>,
    pub draw_state: &'a DrawTables<G::Action>,
    pub parent: usize,
    pub mutation_seed: u64,
    pub mixture_weight: u8,
    pub splice_weight: u8,
}

#[derive(Clone, Debug, Serialize)]
pub struct InputBlockTrace {
    pub work: u64,
    pub before: Option<Vec<u64>>,
    pub after: Option<Vec<u64>>,
    pub would_retain: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InputJobTrace {
    pub blocks: Vec<InputBlockTrace>,
    pub stop_reason: String,
}

pub(crate) fn execute_input_job<G: Workload>(
    context: &InputExecutionContext<'_, G>,
    target: &mut G::Target,
    job: &JobSpec<G>,
    sticky: bool,
    chaining: bool,
    work_limit: u64,
) -> Result<(JobOutcome<G>, InputJobTrace), Box<dyn Error>> {
    if !sticky && !chaining {
        let outcome = run_job(context.workload, context.config, target, job)?;
        let trace = InputJobTrace {
            blocks: vec![InputBlockTrace {
                work: outcome.execution_work,
                before: None,
                after: None,
                would_retain: None,
            }],
            stop_reason: "ordinary_job".into(),
        };
        return Ok((outcome, trace));
    }
    if context.config.window != 1 {
        return Err("alive rollout experiment requires one admission window".into());
    }
    let workload = context.workload;
    let config = context.config;
    let parent = context.parent;
    let before = workload.execution_work(target);
    let prepared = workload.execute_job_certified(
        &config.run,
        target,
        &job.snapshot,
        &job.replay,
        &job.replay_certificates,
        job.parent_milestones,
        &[],
        config.retention,
        config.stop_rollout_on_objective,
    )?;
    if prepared.preparation_failure.is_some() {
        let result_sha256 = workload.certified_result_sha256(&prepared, target)?;
        return Ok((
            JobOutcome {
                reservation: job.reservation,
                result: prepared,
                execution_work: workload.execution_work(target).saturating_sub(before),
                result_sha256,
            },
            InputJobTrace {
                blocks: Vec::new(),
                stop_reason: "preparation_failure".into(),
            },
        ));
    }
    let mut result = prepared;
    let mut milestones = job.parent_milestones;
    let mut previous_key = None;
    let mut suffix = job.suffix.clone();
    let block_len = suffix.len();
    let minimum_work = if chaining {
        workload.continuation_block_work(&config.run)
    } else {
        NonZeroU64::new(
            job.suffix
                .iter()
                .map(|action| {
                    workload
                        .duration_of_action(&config.run, action)
                        .map(NonZeroU64::get)
                })
                .collect::<Option<Vec<_>>>()
                .ok_or("sticky actions need explicit durations")?
                .into_iter()
                .sum(),
        )
        .ok_or("empty input job")?
    };
    let mut sticky_input = if sticky {
        let (previous, mask, longest) = workload
            .sticky_controller(&config.run, context.archive.last_action(parent))
            .ok_or("workload has no sticky controller adapter")?;
        Some(crate::search::sticky_input::StickyInput::new(
            context.mutation_seed ^ u64::from_le_bytes(*b"STICKY!!"),
            previous,
            mask,
            longest,
        ))
    } else {
        None
    };
    if let Some(input) = &mut sticky_input {
        suffix = workload.sticky_actions(&config.run, input, minimum_work)?;
    }
    let mut rand = RomuDuoJrRand::with_seed(context.mutation_seed);
    let mut trace = InputJobTrace {
        blocks: Vec::new(),
        stop_reason: "empty_suffix".into(),
    };
    let mut path = Vec::new();
    loop {
        if suffix.is_empty()
            || workload
                .rollout_outcome(&config.run, target)?
                .should_stop(config.stop_rollout_on_objective)
        {
            break;
        }
        let initial_state = workload.continuation_state(target);
        let block_start = workload.execution_work(target);
        let mut retained = false;
        loop {
            for action in &suffix {
                let mut block = crate::search::rollout::execute_live_suffix(
                    workload,
                    &config.run,
                    target,
                    milestones,
                    std::slice::from_ref(action),
                    config.retention,
                    config.stop_rollout_on_objective,
                )?;
                let Some(last) = block.actions.last() else {
                    break;
                };
                milestones = last.milestones;
                path.push(*action);
                retained = false;
                if let Some(candidate) = &last.candidate {
                    let key =
                        workload.complete_candidate_key(candidate.key, &candidate.snapshot)?;
                    let (kept, completed) =
                        context
                            .archive
                            .would_retain_after(parent, previous_key, &path, key)?;
                    retained = candidate.viable && kept;
                    previous_key = Some(completed);
                }
                let stopped = last.outcome.should_stop(config.stop_rollout_on_objective);
                result.actions.append(&mut block.actions);
                if stopped || workload.execution_work(target).saturating_sub(before) >= work_limit {
                    break;
                }
            }
            if workload.execution_work(target).saturating_sub(block_start) >= minimum_work.get()
                || workload.execution_work(target).saturating_sub(before) >= work_limit
                || workload
                    .rollout_outcome(&config.run, target)?
                    .should_stop(config.stop_rollout_on_objective)
            {
                break;
            }
            if let Some(input) = &mut sticky_input {
                let remaining = minimum_work
                    .get()
                    .saturating_sub(workload.execution_work(target).saturating_sub(block_start));
                suffix = workload.sticky_actions(
                    &config.run,
                    input,
                    NonZeroU64::new(remaining).ok_or("empty sticky refill")?,
                )?;
                continue;
            }
            let seed = rand.next_u64();
            suffix = workload.expand_suffix(
                &config.run,
                context.draw_state,
                MixtureDraw {
                    mixture: config.mixture,
                    weight: context.mixture_weight,
                    splice_weight: context.splice_weight,
                },
                seed,
                path.last(),
            )?;
            workload.refresh_selected_input_timing(&config.run, seed, &mut suffix)?;
            suffix.truncate(block_len);
            if suffix.is_empty() {
                break;
            }
        }
        let final_state = workload.continuation_state(target);
        let changed = initial_state != final_state;
        trace.blocks.push(InputBlockTrace {
            work: workload.execution_work(target).saturating_sub(block_start),
            before: Some(initial_state),
            after: Some(final_state),
            would_retain: Some(retained),
        });
        let stopped = workload
            .rollout_outcome(&config.run, target)?
            .should_stop(config.stop_rollout_on_objective);
        let reason = if stopped {
            Some("terminal_or_objective")
        } else if workload.execution_work(target).saturating_sub(before) >= work_limit {
            Some("work_budget")
        } else if retained {
            Some("retained")
        } else if !chaining {
            Some("single_block")
        } else if !changed {
            Some("unchanged")
        } else {
            None
        };
        if let Some(reason) = reason {
            trace.stop_reason = reason.into();
            break;
        }
        if let Some(input) = &mut sticky_input {
            suffix = workload.sticky_actions(&config.run, input, minimum_work)?;
            continue;
        }
        let seed = rand.next_u64();
        suffix = workload.expand_suffix(
            &config.run,
            context.draw_state,
            MixtureDraw {
                mixture: config.mixture,
                weight: context.mixture_weight,
                splice_weight: context.splice_weight,
            },
            seed,
            path.last(),
        )?;
        workload.refresh_selected_input_timing(&config.run, seed, &mut suffix)?;
        suffix.truncate(block_len);
    }
    workload.verify_integrity()?;
    let execution_work = workload.execution_work(target).saturating_sub(before);
    let result_sha256 = workload.certified_result_sha256(&result, target)?;
    Ok((
        JobOutcome {
            reservation: job.reservation,
            result,
            execution_work,
            result_sha256,
        },
        trace,
    ))
}
