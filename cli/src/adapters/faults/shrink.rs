// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{investigate, runs::Manifest};
use crate::{adapters::Request, config::Result};
use faults_workload::{
    FaultAction,
    campaign::FaultCampaignRun,
    package::{self, Report},
};
use searcher::search::shrink::{Improvement, ShrinkEvent, ShrinkProgress, ShrinkStop};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub const SHRINK_FORMAT: &str = "harmony-fault-shrink-v2";

#[derive(Deserialize, Serialize)]
struct Summary {
    format: String,
    finding: Option<usize>,
    violations: Vec<String>,
    seed: u64,
    workers: usize,
    faults_before: usize,
    faults_after: usize,
    ticks_before: u64,
    ticks_after: u64,
    replays: u64,
    stop: ShrinkStop,
    progress: ShrinkProgress,
    improvements: Vec<Improvement>,
    actions: Vec<FaultAction>,
}

pub struct Shrunk {
    pub report: Report,
    pub actions: Vec<FaultAction>,
    pub replays: u64,
    pub faults_before: usize,
}

pub fn faults(actions: &[FaultAction]) -> usize {
    actions.iter().filter(|action| action.is_fault()).count()
}

fn ticks(actions: &[FaultAction]) -> u64 {
    actions.iter().map(FaultAction::ticks).sum()
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
    if recorded.violations.is_empty() {
        return Err(format!(
            "the selected execution stopped with {:?} and names no violation to {verb}",
            recorded.stop
        )
        .into());
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

pub fn shrunk_output(path: &Path) -> Result<bool> {
    let file = path.join("shrink.json");
    if !file.is_file() {
        return Ok(false);
    }
    let summary: serde_json::Value = serde_json::from_slice(&fs::read(&file)?)?;
    if summary["format"] != SHRINK_FORMAT {
        return Err(format!(
            "{} was written by an older harmony shrink; shrink the finding again or pass --no-shrink",
            file.display()
        )
        .into());
    }
    Ok(true)
}

pub fn shrink_into(
    out: &Path,
    manifest: &Manifest,
    source: &Source,
    finding: Option<usize>,
    jobs: usize,
) -> Result<Shrunk> {
    let artifacts = manifest.fault_artifacts(out)?;
    let options = manifest.options(out)?;
    let run = FaultCampaignRun {
        vocabulary: super::workflow::read_vocabulary(&source.path, &source.manifest)?,
        violations: source.violations.iter().cloned().collect::<BTreeSet<_>>(),
    };
    let faults_before = faults(&source.actions);
    let shrunk = package::shrink(&artifacts, &options, run, &source.actions, jobs, |event| {
        if let ShrinkEvent::Improved { input, improvement } = event {
            eprintln!(
                "shrink job {}: {} faults, {} ticks, {} actions",
                improvement.job,
                faults(&input.actions),
                improvement.cost.secondary,
                input.actions.len()
            );
        }
        Ok(())
    })?;
    let outcome = shrunk.outcome;
    let actions = outcome.input.actions;
    let report = package::execute_actions(&artifacts, &actions, 1, &options, manifest.settle)?;
    let replay = report
        .replays
        .first()
        .ok_or("the shrunk replay has no result")?;
    if !replay.bug
        || !replay
            .violations
            .iter()
            .any(|violation| source.violations.contains(violation))
    {
        return Err(format!(
            "the shrunk actions replayed without {:?}; see {}",
            source.violations,
            out.join("report.json").display()
        )
        .into());
    }
    let replays = outcome
        .progress
        .jobs
        .total()
        .saturating_add(outcome.progress.rebuilds)
        .saturating_add(2);
    fs::write(
        out.join("shrink.json"),
        serde_json::to_vec_pretty(&Summary {
            format: SHRINK_FORMAT.to_owned(),
            finding,
            violations: source.violations.clone(),
            seed: options.seed,
            workers: shrunk.workers,
            faults_before,
            faults_after: faults(&actions),
            ticks_before: ticks(&source.actions),
            ticks_after: ticks(&actions),
            replays,
            stop: outcome.stop,
            progress: outcome.progress,
            improvements: outcome.improvements,
            actions: actions.clone(),
        })?,
    )?;
    Ok(Shrunk {
        report,
        actions,
        replays,
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
            "kept {} of {} faults and {} of {} ticks after {} replays; violation: {}",
            faults(&shrunk.actions),
            shrunk.faults_before,
            ticks(&shrunk.actions),
            ticks(&source.actions),
            shrunk.replays,
            source.violations.join("; ")
        );
        for (index, action) in shrunk.actions.iter().enumerate() {
            if action.is_fault() {
                println!("  step {}: {:?}", index + 1, action.operation);
            }
        }
        Ok(0)
    })();
    super::workflow::finish(&out, &mut manifest, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_shrink_output_is_refused_and_the_current_one_is_recognized() {
        let directory = tempfile::tempdir().unwrap();
        assert!(!shrunk_output(directory.path()).unwrap());
        fs::write(
            directory.path().join("shrink.json"),
            br#"{"finding": 1, "replaced": [0], "replays": 3}"#,
        )
        .unwrap();
        let error = shrunk_output(directory.path()).unwrap_err().to_string();
        assert!(error.contains("older harmony shrink"), "{error}");
        fs::write(
            directory.path().join("shrink.json"),
            serde_json::to_vec(&serde_json::json!({ "format": SHRINK_FORMAT })).unwrap(),
        )
        .unwrap();
        assert!(shrunk_output(directory.path()).unwrap());
    }
}
