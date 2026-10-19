// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{investigate, runs::Manifest};
use crate::{adapters::Request, config::Result};
use faults_workload::{FaultAction, FaultOperation, package::Report};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    num::NonZeroU16,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Attempt {
    pub replaced: Vec<usize>,
    pub reproduced: bool,
    pub violations: Vec<String>,
}

pub struct Trial<T> {
    pub reproduced: bool,
    pub violations: Vec<String>,
    pub value: T,
}

pub struct Reduction<T> {
    pub replaced: BTreeSet<usize>,
    pub attempts: Vec<Attempt>,
    pub value: T,
}

pub fn as_wait(action: FaultAction) -> FaultAction {
    let ticks = u16::try_from(action.ticks())
        .ok()
        .and_then(NonZeroU16::new)
        .unwrap_or(NonZeroU16::MIN);
    FaultAction {
        operation: FaultOperation::Wait(ticks),
        ..action
    }
}

pub fn candidate(actions: &[FaultAction], replaced: &BTreeSet<usize>) -> Vec<FaultAction> {
    actions
        .iter()
        .enumerate()
        .map(|(index, action)| {
            if replaced.contains(&index) {
                as_wait(*action)
            } else {
                *action
            }
        })
        .collect()
}

fn run_batch<T: Send>(
    sets: &[BTreeSet<usize>],
    actions: &[FaultAction],
    first: usize,
    evaluate: &(impl Fn(usize, &[FaultAction]) -> std::result::Result<Trial<T>, String> + Sync),
) -> Result<Vec<Trial<T>>> {
    let outcomes = std::thread::scope(|scope| {
        let handles = sets
            .iter()
            .enumerate()
            .map(|(offset, set)| {
                let input = candidate(actions, set);
                scope.spawn(move || evaluate(first + offset, &input))
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| Err("a shrink attempt panicked".into()))
            })
            .collect::<Vec<_>>()
    });
    outcomes
        .into_iter()
        .map(|outcome| outcome.map_err(Into::into))
        .collect()
}

pub fn reduce<T: Send>(
    actions: &[FaultAction],
    jobs: usize,
    baseline: T,
    evaluate: impl Fn(usize, &[FaultAction]) -> std::result::Result<Trial<T>, String> + Sync,
) -> Result<Reduction<T>> {
    let mut kept = actions
        .iter()
        .enumerate()
        .filter(|(_, action)| !matches!(action.operation, FaultOperation::Wait(_)))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let mut reduction = Reduction {
        replaced: BTreeSet::new(),
        attempts: Vec::new(),
        value: baseline,
    };
    let mut parts = 2;
    let mut next = 1;
    while !kept.is_empty() {
        let parts_now = parts.min(kept.len());
        let size = kept.len().div_ceil(parts_now);
        let chunks = kept.chunks(size).map(<[usize]>::to_vec).collect::<Vec<_>>();
        let mut accepted = None;
        for batch in chunks.chunks(jobs.max(1)) {
            let sets = batch
                .iter()
                .map(|chunk| {
                    let mut set = reduction.replaced.clone();
                    set.extend(chunk);
                    set
                })
                .collect::<Vec<_>>();
            let trials = run_batch(&sets, actions, next, &evaluate)?;
            next += sets.len();
            for ((chunk, set), trial) in batch.iter().zip(&sets).zip(trials) {
                reduction.attempts.push(Attempt {
                    replaced: set.iter().copied().collect(),
                    reproduced: trial.reproduced,
                    violations: trial.violations,
                });
                if trial.reproduced && accepted.is_none() {
                    accepted = Some((chunk.clone(), trial.value));
                }
            }
            if accepted.is_some() {
                break;
            }
        }
        match accepted {
            Some((chunk, value)) => {
                reduction.replaced.extend(&chunk);
                kept.retain(|index| !chunk.contains(index));
                reduction.value = value;
                parts = parts_now.saturating_sub(1).max(2);
            }
            None if size == 1 => break,
            None => parts = (parts_now * 2).min(kept.len()),
        }
    }
    Ok(reduction)
}

fn reproduces(report: &Report, target: &[String]) -> (bool, Vec<String>) {
    let violations = report
        .replays
        .first()
        .map(|replay| replay.violations.clone())
        .unwrap_or_default();
    let reproduced = report.replays.first().is_some_and(|replay| replay.bug)
        && violations
            .iter()
            .any(|violation| target.contains(violation));
    (reproduced, violations)
}

#[derive(Serialize)]
struct Summary<'a> {
    finding: Option<usize>,
    violations: &'a [String],
    faults_before: usize,
    faults_after: usize,
    replaced: Vec<usize>,
    attempts: &'a [Attempt],
    actions: &'a [FaultAction],
}

pub struct Shrunk {
    pub report: Report,
    pub actions: Vec<FaultAction>,
    pub replays: usize,
    pub faults_before: usize,
}

pub fn faults(actions: &[FaultAction]) -> usize {
    actions
        .iter()
        .filter(|action| !matches!(action.operation, FaultOperation::Wait(_)))
        .count()
}

pub struct Source {
    pub manifest: Manifest,
    pub path: PathBuf,
    pub actions: Vec<FaultAction>,
    pub violations: Vec<String>,
}

pub fn source(saved: crate::runs::Saved, finding: Option<usize>, verb: &str) -> Result<Source> {
    let path = saved.path;
    let manifest = Manifest::from_record(saved.manifest)?;
    manifest.verify(&path)?;
    if manifest.bundle.is_none() {
        return Err(format!("{verb} requires a supervised application scenario").into());
    }
    let recorded = investigate::recorded_witness(&manifest, finding)?;
    if !recorded.bug {
        return Err(format!("the selected execution has no violation to {verb}").into());
    }
    let (mut actions, _, anchor) = investigate::trajectory(&recorded, true);
    actions.truncate(anchor);
    Ok(Source {
        manifest,
        path,
        actions,
        violations: recorded.violations,
    })
}

pub fn shrink_into(
    out: &Path,
    manifest: &Manifest,
    source: &Source,
    finding: Option<usize>,
    jobs: usize,
) -> Result<Shrunk> {
    let actions = &source.actions;
    let target = &source.violations;
    let artifacts = manifest.fault_artifacts(out)?;
    let options = manifest.options(out)?;
    let settle = source.manifest.settle;
    let evaluate =
        |index: usize, input: &[FaultAction]| -> std::result::Result<Trial<Report>, String> {
            let mut options = options.clone();
            options.output = attempt_dir(out, index);
            fs::create_dir_all(&options.output).map_err(|error| error.to_string())?;
            let report =
                faults_workload::package::execute_actions(&artifacts, input, 1, &options, settle)
                    .map_err(|error| error.to_string())?;
            let (reproduced, violations) = reproduces(&report, target);
            Ok(Trial {
                reproduced,
                violations,
                value: report,
            })
        };
    let baseline = evaluate(0, actions)?;
    if !baseline.reproduced {
        return Err(format!(
            "the recorded actions no longer produce {target:?}; see {}",
            attempt_dir(out, 0).display()
        )
        .into());
    }
    let reduction = reduce(actions, jobs.max(1), baseline.value, evaluate)?;
    let shrunk = candidate(actions, &reduction.replaced);
    let faults_before = faults(actions);
    fs::write(
        out.join("shrink.json"),
        serde_json::to_vec_pretty(&Summary {
            finding,
            violations: target,
            faults_before,
            faults_after: faults_before - reduction.replaced.len(),
            replaced: reduction.replaced.iter().copied().collect(),
            attempts: &reduction.attempts,
            actions: &shrunk,
        })?,
    )?;
    Ok(Shrunk {
        report: reduction.value,
        actions: shrunk,
        replays: reduction.attempts.len() + 1,
        faults_before,
    })
}

#[cfg(feature = "frontend")]
pub fn shrink(request: Request) -> Result<u8> {
    let saved = crate::runs::Saved::read(
        &request
            .selection
            .as_ref()
            .ok_or("select a search or branch")?
            .run,
    )?;
    shrink_saved(request, saved)
}

pub fn shrink_saved(request: Request, saved: crate::runs::Saved) -> Result<u8> {
    let selection = request.selection.ok_or("select a search or branch")?;
    let source = source(saved, selection.finding, "shrink")?;
    let out = request.destination.create()?;
    let mut manifest = source.manifest.inherit(&source.path, &out, "replay")?;
    manifest.actions = source.actions.clone();
    manifest.save(&out)?;
    let result = (|| -> Result<u8> {
        let shrunk = shrink_into(&out, &manifest, &source, selection.finding, request.jobs)?;
        manifest.actions = shrunk.actions.clone();
        shrunk.report.write(&out)?;
        manifest.pin_outcome(&shrunk.report)?;
        println!(
            "kept {} of {} faults after {} replays; violation: {}",
            faults(&shrunk.actions),
            shrunk.faults_before,
            shrunk.replays,
            source.violations.join("; ")
        );
        for (index, action) in shrunk.actions.iter().enumerate() {
            if !matches!(action.operation, FaultOperation::Wait(_)) {
                println!("  step {}: {:?}", index + 1, action.operation);
            }
        }
        Ok(0)
    })();
    super::workflow::finish(&out, &mut manifest, result)
}

fn attempt_dir(out: &Path, index: usize) -> PathBuf {
    out.join("attempts").join(index.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(operation: FaultOperation, choice: u64) -> FaultAction {
        FaultAction::new(operation, NonZeroU16::MIN).with_choice(choice)
    }

    fn ticks(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).unwrap()
    }

    fn schedule() -> Vec<FaultAction> {
        vec![
            action(FaultOperation::Kill(1, ticks(256)), 11),
            action(FaultOperation::Wait(ticks(256)), 12),
            action(FaultOperation::Pause(2, ticks(128)), 13),
            action(FaultOperation::KillAll(ticks(256)), 14),
            action(FaultOperation::Restart(0, ticks(64)), 15),
            action(FaultOperation::Hook(1, ticks(256)), 16),
            action(FaultOperation::Wait(ticks(256)), 17),
        ]
    }

    fn needs(
        required: &'static [usize],
    ) -> impl Fn(usize, &[FaultAction]) -> std::result::Result<Trial<usize>, String> + Sync {
        move |index, input| {
            let reproduced = required
                .iter()
                .all(|step| !matches!(input[*step].operation, FaultOperation::Wait(_)));
            Ok(Trial {
                reproduced,
                violations: if reproduced {
                    vec!["lost".into()]
                } else {
                    Vec::new()
                },
                value: index,
            })
        }
    }

    #[test]
    fn replaced_faults_keep_their_length_and_choice() {
        let actions = schedule();
        let shrunk = candidate(&actions, &BTreeSet::from([0, 3]));
        assert_eq!(shrunk[0].operation, FaultOperation::Wait(ticks(256)));
        assert_eq!(shrunk[0].choice, 11);
        assert_eq!(shrunk[0].coverage_quantum, actions[0].coverage_quantum);
        assert_eq!(shrunk[3].operation, FaultOperation::Wait(ticks(256)));
        assert_eq!(shrunk[2], actions[2]);
        assert_eq!(
            shrunk.iter().map(FaultAction::ticks).sum::<u64>(),
            actions.iter().map(FaultAction::ticks).sum::<u64>()
        );
    }

    #[test]
    fn reduction_keeps_only_the_faults_the_violation_needs() {
        let actions = schedule();
        for jobs in [1, 2, 8] {
            let reduction = reduce(&actions, jobs, 0, needs(&[0, 3])).unwrap();
            assert_eq!(reduction.replaced, BTreeSet::from([2, 4, 5]), "jobs {jobs}");
            assert!(reduction.attempts.iter().all(|attempt| {
                !attempt.replaced.contains(&1) && !attempt.replaced.contains(&6)
            }));
        }
    }

    #[test]
    fn parallel_batches_accept_the_same_candidates_as_one_job() {
        let actions = schedule();
        let replaced = |jobs| reduce(&actions, jobs, 0, needs(&[4])).unwrap().replaced;
        let single = replaced(1);
        assert_eq!(single, BTreeSet::from([0, 2, 3, 5]));
        assert_eq!(single, replaced(4));
    }

    #[test]
    fn a_violation_without_faults_replaces_every_fault() {
        let reduction = reduce(&schedule(), 3, 0, needs(&[])).unwrap();
        assert_eq!(reduction.replaced, BTreeSet::from([0, 2, 3, 4, 5]));
    }

    #[test]
    fn evaluation_errors_stop_the_reduction() {
        let error = reduce(
            &schedule(),
            2,
            0,
            |_, _| -> std::result::Result<Trial<usize>, String> { Err("guest failed".into()) },
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("guest failed"), "{error}");
    }
}
