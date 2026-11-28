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

pub const SHRINK_FORMAT: &str = "harmony-fault-shrink-v3";

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
    let manifest = Manifest::from_record(saved.manifest, &path)?;
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
        .saturating_add(outcome.progress.accepted.total())
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
    use crate::{runs::Destination, selection::Selection};
    use faults_workload::{
        FaultOperation,
        package::{ReplayStep, ReplaySummary},
        target::{FaultObservations, FaultStop},
    };
    use std::num::NonZeroU16;

    fn ticks_of(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).unwrap()
    }

    fn action(operation: FaultOperation) -> FaultAction {
        FaultAction::new(operation, ticks_of(1))
    }

    fn step(step: u64, action: Option<FaultAction>, stop: FaultStop) -> ReplayStep {
        ReplayStep {
            step,
            action,
            observation: FaultObservations {
                stop,
                ..FaultObservations::default()
            },
            console: String::new(),
            settlement: false,
            choices: 0,
        }
    }

    fn witness(bug: bool, violations: &[&str]) -> ReplaySummary {
        let mut summary =
            ReplaySummary::from_observation(&FaultObservations::default(), Some([7; 32]), 3, 0);
        summary.bug = bug;
        summary.stop = if bug {
            FaultStop::Crash
        } else {
            FaultStop::Deadline
        };
        summary.violations = violations.iter().map(|text| (*text).to_owned()).collect();
        summary.timeline = vec![
            step(0, None, FaultStop::Deadline),
            step(
                1,
                Some(action(FaultOperation::Kill(0, ticks_of(2)))),
                FaultStop::Deadline,
            ),
            step(
                2,
                Some(action(FaultOperation::Wait(ticks_of(5)))),
                FaultStop::Crash,
            ),
            step(
                3,
                Some(action(FaultOperation::Pause(1, ticks_of(4)))),
                FaultStop::Crash,
            ),
        ];
        summary
    }

    fn saved_run(path: &Path, bundle: bool, replay: ReplaySummary) {
        let options = faults_workload::Options {
            seed: 0,
            executions: 1,
            ram_mib: 1024,
            knobs: Vec::new(),
            wall_seconds: None,
            output: path.to_path_buf(),
            uml_profile: None,
            root: None,
            checkpoint_owner: None,
            boot_cache: None,
            key: faults_workload::key::KeyRequest::default(),
            answers: Vec::new(),
        };
        let mut report = Report::new(
            "replay",
            &faults_workload::Artifacts {
                kernel: Vec::new(),
                initramfs: Vec::new(),
            },
            "runner".into(),
            &options,
            1,
        );
        report.replays.push(replay);
        report.write(path).unwrap();
        let mut config = crate::config::Config::default();
        config.runner.kind = "test".into();
        config.workload.package = "faults".into();
        let mut record = crate::runs::Manifest::new(config, "run").unwrap();
        record.store(path, "kernel", b"").unwrap();
        record.store(path, "initramfs", b"").unwrap();
        record.status = "complete".into();
        let outcome = serde_json::to_value(&report).unwrap();
        if !bundle {
            fs::write(path.join("run.json"), serde_json::to_vec(&outcome).unwrap()).unwrap();
        }
        record.payload = serde_json::json!({
            "bundle": bundle.then_some("node app \"/bin/app\"\n"),
            "actions": [],
            "settle": true,
            "outcome": outcome
        });
        record.save(path).unwrap();
    }

    fn source_error(bundle: bool, replay: ReplaySummary) -> String {
        let directory = tempfile::tempdir().unwrap();
        saved_run(directory.path(), bundle, replay);
        let saved = crate::runs::Saved::read(directory.path().to_str().unwrap()).unwrap();
        match source(saved, None, "shrink") {
            Ok(_) => panic!("the source was accepted"),
            Err(error) => error.to_string(),
        }
    }

    fn shrink_request(run: &Path, out: &Path) -> Request {
        Request {
            selection: Some(Selection {
                run: run.display().to_string(),
                finding: None,
            }),
            destination: Destination {
                name: None,
                out: Some(out.to_path_buf()),
            },
            ..Request::default()
        }
    }

    #[test]
    fn faults_and_ticks_count_only_what_the_actions_hold() {
        let actions = [
            action(FaultOperation::Wait(ticks_of(5))),
            action(FaultOperation::Kill(0, ticks_of(2))),
            action(FaultOperation::Pause(1, ticks_of(4))),
        ];
        assert_eq!(faults(&actions), 2);
        assert_eq!(ticks(&actions), 11);
        assert_eq!(faults(&actions[..1]), 0);
        assert_eq!(ticks(&[]), 0);
    }

    #[test]
    fn a_source_keeps_the_actions_through_the_first_violating_step() {
        let directory = tempfile::tempdir().unwrap();
        saved_run(directory.path(), true, witness(true, &["linearizable"]));
        let saved = crate::runs::Saved::read(directory.path().to_str().unwrap()).unwrap();
        let source = source(saved, None, "shrink").unwrap();
        assert_eq!(source.path, directory.path().canonicalize().unwrap());
        assert_eq!(source.violations, ["linearizable"]);
        assert_eq!(
            source.actions,
            [
                action(FaultOperation::Kill(0, ticks_of(2))),
                action(FaultOperation::Wait(ticks_of(5))),
            ]
        );
    }

    #[test]
    fn a_source_without_a_bundle_or_a_violation_is_refused() {
        assert_eq!(
            source_error(false, witness(true, &["linearizable"])),
            "shrink requires a supervised application scenario"
        );
        assert_eq!(
            source_error(false, witness(false, &[])),
            "shrink requires a supervised application scenario"
        );
        assert_eq!(
            source_error(true, witness(false, &[])),
            "the selected execution has no violation to shrink"
        );
        assert_eq!(
            source_error(true, witness(true, &[])),
            "the selected execution stopped with Crash and names no violation to shrink"
        );
    }

    #[test]
    fn a_source_whose_recorded_report_changed_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        saved_run(directory.path(), true, witness(true, &["linearizable"]));
        let saved = crate::runs::Saved::read(directory.path().to_str().unwrap()).unwrap();
        fs::write(directory.path().join("report.json"), b"{}").unwrap();
        let error = match source(saved, None, "shrink") {
            Ok(_) => panic!("the changed report was accepted"),
            Err(error) => error.to_string(),
        };
        assert_eq!(error, "recorded report.json has changed");
    }

    #[test]
    fn shrink_saved_refuses_before_creating_its_destination() {
        let directory = tempfile::tempdir().unwrap();
        saved_run(directory.path(), true, witness(false, &[]));
        let out = directory.path().join("child");
        let saved = crate::runs::Saved::read(directory.path().to_str().unwrap()).unwrap();
        let error = shrink_saved(shrink_request(directory.path(), &out), saved)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "the selected execution has no violation to shrink");
        assert!(!out.exists());

        let saved = crate::runs::Saved::read(directory.path().to_str().unwrap()).unwrap();
        let error = shrink_saved(Request::default(), saved)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "select a search or branch");
    }

    #[cfg(feature = "frontend")]
    #[test]
    fn shrink_reads_the_selected_run_and_requires_a_selection() {
        let error = shrink(Request::default()).unwrap_err().to_string();
        assert_eq!(error, "select a search or branch");

        let directory = tempfile::tempdir().unwrap();
        saved_run(directory.path(), false, witness(true, &["linearizable"]));
        let out = directory.path().join("child");
        let error = shrink(shrink_request(directory.path(), &out))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "shrink requires a supervised application scenario");
        assert!(!out.exists());
    }

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
