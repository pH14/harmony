// SPDX-License-Identifier: AGPL-3.0-or-later

use super::runs::Manifest;
use crate::{config::Result, runs::Destination};
use faults_workload::{
    FaultAction,
    package::{self, ReplaySummary, Report, StateHashEncoding},
};
use std::fs;

use crate::selection::Point;
pub use crate::selection::Selection;

fn recorded_report(manifest: &Manifest) -> Result<Report> {
    Ok(serde_json::from_value(
        manifest.record.payload["outcome"].clone(),
    )?)
}

fn bug_index(bug: usize, length: usize) -> Result<usize> {
    bug.checked_sub(1).filter(|i| *i < length).ok_or_else(|| {
        format!("bug {bug} does not exist; this recording has {length} findings").into()
    })
}

pub fn selected_actions(manifest: &Manifest, bug: Option<usize>) -> Result<Vec<FaultAction>> {
    if let Some(bug) = bug {
        let report = recorded_report(manifest)?;
        Ok(report.bugs[bug_index(bug, report.bugs.len())?]
            .actions
            .clone())
    } else if manifest.mode == "search" {
        Err(
            "select a finding with --finding NUMBER; use search --resume NAME to continue the whole search"
                .into(),
        )
    } else {
        Ok(manifest.actions.clone())
    }
}

fn report_witness(report: &Report, bug: Option<usize>) -> Result<ReplaySummary> {
    if let Some(bug) = bug {
        report.bugs[bug_index(bug, report.bugs.len())?]
            .replay
            .clone()
            .ok_or_else(|| "this finding has no successful replay evidence".into())
    } else {
        report
            .replays
            .first()
            .cloned()
            .ok_or_else(|| "select a finding with --finding NUMBER".into())
    }
}

fn recorded_witness(manifest: &Manifest, bug: Option<usize>) -> Result<ReplaySummary> {
    report_witness(&recorded_report(manifest)?, bug)
}

fn equivalent(expected: &ReplaySummary, actual: &ReplaySummary) -> bool {
    expected.state_hash_encoding != StateHashEncoding::Unavailable
        && actual.state_hash_encoding != StateHashEncoding::Unavailable
        && expected.state_hash == actual.state_hash
        && expected.state_hash_encoding == actual.state_hash_encoding
        && expected.bug == actual.bug
        && expected.stop == actual.stop
        && expected.violations == actual.violations
        && expected.sometimes == actual.sometimes
        && expected.actions_applied == actual.actions_applied
        && expected.settle_actions == actual.settle_actions
        && expected.settle_ticks == actual.settle_ticks
        && expected.event_kill_fires == actual.event_kill_fires
        && expected.event_park_fires == actual.event_park_fires
        && expected.check == actual.check
        && expected.timeline == actual.timeline
}

fn command_replay_matches(original: &Manifest, actual: serde_json::Value) -> bool {
    crate::oci::equivalent_record(
        original.record.payload["outcome"].clone(),
        actual,
        original.config.backend,
    )
}

#[cfg(feature = "frontend")]
pub fn replay(selection: Selection, destination: Destination, repeat: u32) -> Result<u8> {
    let saved = crate::runs::Saved::read(&selection.run)?;
    replay_saved(selection, destination, repeat, saved)
}

pub fn replay_saved(
    selection: Selection,
    destination: Destination,
    repeat: u32,
    saved: crate::runs::Saved,
) -> Result<u8> {
    if repeat == 0 {
        return Err("repeat must be positive".into());
    }
    let source = saved.path;
    let original = Manifest::from_record(saved.manifest)?;
    original.verify(&source)?;
    if original.status != "complete" {
        return Err("a failed or incomplete recording cannot certify exact replay".into());
    }
    if original.bundle.is_some()
        && let Some(finding) = selection.finding
    {
        let recorded_report = recorded_report(&original)?;
        let bug = &recorded_report.bugs[bug_index(finding, recorded_report.bugs.len())?];
        if !bug.confirmed {
            return Err("this finding was not confirmed by replay".into());
        }
    }
    let recorded = if original.bundle.is_some() {
        let actions = selected_actions(&original, selection.finding)?;
        let report = recorded_report(&original)?;
        let expected = report_witness(&report, selection.finding)?;
        if selection.finding.is_none()
            && expected.state_hash_encoding != StateHashEncoding::Unavailable
            && report
                .replays
                .iter()
                .skip(1)
                .any(|saved| !equivalent(&expected, saved))
        {
            return Err("saved repeats disagree; exact replay cannot be verified".into());
        }
        Some((actions, expected))
    } else {
        if selection.finding.is_some() || repeat != 1 {
            return Err("command runs have no finding selector and replay once".into());
        }
        None
    };
    let out = destination.create()?;
    let mut manifest = original.inherit(&source, &out, "replay")?;
    let result = (|| -> Result<u8> {
        if original.bundle.is_none() {
            manifest.save(&out)?;
            let mut c = manifest.config.clone();
            if manifest.uml_host.is_some() {
                c.uml_profile = Some(out.join("artifacts/uml"));
            }
            let artifacts = manifest.fault_artifacts(&out)?;
            let profile_identity = manifest.uml_profile_identity(&out)?;
            let (_, new) = crate::oci::execute(
                &c.runtime(),
                &manifest.runner_identity,
                profile_identity.as_deref(),
                c.seed,
                &c.knobs,
                c.wall_seconds,
                &artifacts.kernel,
                &artifacts.initramfs,
                &out,
                false,
            )?;
            manifest.pin_outcome(&new)?;
            if !command_replay_matches(&original, new) {
                Err("replay diverged from the recorded command execution".into())
            } else {
                Ok(0)
            }
        } else {
            let (actions, expected) = recorded.ok_or("missing recorded execution")?;
            manifest.actions = actions;
            manifest.save(&out)?;
            let artifacts = manifest.fault_artifacts(&out)?;
            let replay = package::execute_actions(
                &artifacts,
                &manifest.actions,
                repeat,
                &manifest.options(&out)?,
                original.settle,
            )?;
            manifest.pin_outcome(&replay)?;
            if expected.state_hash_encoding == StateHashEncoding::Unavailable
                || replay
                    .replays
                    .iter()
                    .any(|actual| actual.state_hash_encoding == StateHashEncoding::Unavailable)
            {
                Err("full UML state identity is unavailable; replay observations were recorded for inspection, but exact reproduction cannot be verified".into())
            } else if replay
                .replays
                .iter()
                .all(|actual| equivalent(&expected, actual))
            {
                println!("verified {repeat} fresh replay(s)");
                Ok(0)
            } else {
                Err("replay diverged; inspect the new run for the actual observations".into())
            }
        }
    })();
    super::workflow::finish(&out, &mut manifest, result)
}

#[cfg(feature = "frontend")]
pub fn branch(request: crate::adapters::Request) -> Result<u8> {
    let saved = crate::runs::Saved::read(
        &request
            .selection
            .as_ref()
            .ok_or("select a search or branch")?
            .run,
    )?;
    branch_saved(request, saved)
}

pub fn branch_saved(request: crate::adapters::Request, saved: crate::runs::Saved) -> Result<u8> {
    let selection = request.selection.ok_or("select a search or branch")?;
    let source = saved.path;
    let original = Manifest::from_record(saved.manifest)?;
    original.verify(&source)?;
    if original.bundle.is_none() {
        return Err("branching requires a supervised application scenario".into());
    }
    let (actions, boundaries, anchor) = if selection.finding.is_none()
        && request.point.step == Some(0)
        && original.mode == "search"
    {
        (Vec::new(), vec![(0, 0)], 0)
    } else {
        let recorded = recorded_witness(&original, selection.finding)?;
        trajectory(&recorded, selection.finding.is_some())
    };
    let cut = request.point.resolve(&boundaries, anchor)?;
    if cut > actions.len() {
        return Err("selected boundary has no reproducible input prefix".into());
    }
    let script = match (&request.exec, &request.exec_file) {
        (Some(text), None) => Some(text.as_bytes().to_vec()),
        (None, Some(path)) => Some(fs::read(path)?),
        (None, None) => None,
        _ => return Err("choose --exec or --exec-file".into()),
    };
    if script
        .as_ref()
        .is_some_and(|s| s.len() > process_proto::debug::MAX_SCRIPT)
    {
        return Err("guest script exceeds 1 MiB".into());
    }
    let mut continuation = Vec::new();
    if let Some(path) = request.actions {
        continuation
            .extend(faults_workload::parse_recorded_input(&fs::read_to_string(path)?)?.actions);
    }
    let stop = request.stop || request.shell;
    if !stop {
        continuation.extend_from_slice(&actions[cut..]);
    }
    let out = request.destination.create()?;
    let mut manifest = original.inherit(&source, &out, "branch")?;
    manifest.actions = continuation;
    manifest.settle = !stop;
    manifest.save(&out)?;
    println!("branch point: after action {cut}");
    let result = (|| -> Result<u8> {
        let mut target = package::prepare_debug(
            &manifest.fault_artifacts(&out)?,
            &actions[..cut],
            &manifest.options(&out)?,
        )?;
        if let Some(script) = &script {
            manifest.store(&out, "debug-script.sh", script)?;
        }
        let code = super::terminal::execute(&mut target, &out, script.as_deref(), request.shell)?;
        let root = target.export_root()?;
        drop(target);
        manifest.store(&out, "root", &root)?;
        fs::write(
            out.join("branch.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"parent_step":cut,"shell":request.shell,"command_exit":code}),
            )?,
        )?;
        manifest.save(&out)?;
        let report = package::execute_actions(
            &manifest.fault_artifacts(&out)?,
            &manifest.actions,
            1,
            &manifest.options(&out)?,
            !stop,
        )?;
        manifest.pin_outcome(&report)?;
        Ok(if code != 0 {
            1
        } else {
            super::workflow::report_code(&report)
        })
    })();
    super::workflow::finish(&out, &mut manifest, result)
}

pub fn trajectory(
    recorded: &ReplaySummary,
    finding: bool,
) -> (Vec<FaultAction>, Vec<(usize, u64)>, usize) {
    let applied = recorded.actions_applied + recorded.settle_actions;
    let mut actions = Vec::new();
    let mut boundaries = Vec::new();
    for step in &recorded.timeline {
        if step.step > applied
            || boundaries
                .last()
                .is_some_and(|(n, _)| *n == step.step as usize)
        {
            continue;
        }
        boundaries.push((step.step as usize, step.observation.moment));
        if let Some(action) = step.action {
            actions.push(action);
        }
    }
    let end = boundaries.last().map_or(0, |(n, _)| *n);
    let anchor = if finding {
        recorded
            .timeline
            .iter()
            .find(|step| step.step <= applied && step.observation.is_bug())
            .map_or(end, |step| step.step as usize)
    } else {
        end
    };
    (actions, boundaries, anchor)
}
pub fn prefix(
    manifest: &Manifest,
    selection: &Selection,
    point: &Point,
) -> Result<Vec<FaultAction>> {
    if manifest.mode != "branch" {
        return Err("search --from requires a saved branch".into());
    }
    let recorded = recorded_witness(manifest, selection.finding)?;
    let (actions, boundaries, anchor) = trajectory(&recorded, selection.finding.is_some());
    let cut = point.resolve(&boundaries, anchor)?;
    if cut > actions.len() {
        return Err("point has no reproducible input prefix".into());
    }
    eprintln!("selected step {cut}");
    Ok(actions[..cut].to_vec())
}

#[cfg(feature = "frontend")]
pub fn inspect(selection: Selection, json: bool) -> Result<u8> {
    let path = crate::runs::locate(&selection.run)?;
    let manifest = Manifest::read(&path)?;
    let report = manifest.record.payload["outcome"].clone();
    let selected = if let Some(bug) = selection.finding {
        let bugs = report["bugs"]
            .as_array()
            .ok_or("this recording has no findings")?;
        bugs[bug_index(bug, bugs.len())?].clone()
    } else {
        report
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "manifest": manifest, "result": selected })
            )?
        );
    } else {
        println!(
            "{}: {} ({}, {:?})",
            path.display(),
            manifest.status,
            manifest.mode,
            manifest.config.backend
        );
        if let Some(error) = &manifest.error {
            println!("error: {error}");
        }
        let show_replay = |replay: &serde_json::Value| {
            println!(
                "actions: {}; recovery actions: {}; stop: {}; state: {}",
                replay["actions_applied"],
                replay["settle_actions"],
                replay["stop"],
                if replay["state_hash_encoding"] == "unavailable" {
                    "unavailable".to_owned()
                } else {
                    replay["state_hash"].to_string()
                }
            );
            for violation in replay["violations"].as_array().into_iter().flatten() {
                println!("  violation: {}", violation.as_str().unwrap_or_default());
            }
        };
        if let Some(bug) = selection.finding {
            println!(
                "finding {bug}: confirmed={}; execution={}",
                selected["confirmed"], selected["execution"]
            );
            show_replay(&selected["replay"]);
            println!(
                "timeline: harmony show {} --timeline --finding {bug}\nbranch: harmony branch {} --finding {bug}",
                path.display(),
                path.display()
            );
        } else if let Some(bugs) = selected["bugs"].as_array() {
            println!(
                "executions: {}; failures: {}; watchdog cutoffs: {}",
                selected["executions"],
                selected["execution_failures"],
                selected["watchdog_cutoffs"]
            );
            for (index, bug) in bugs.iter().enumerate() {
                println!(
                    "finding {}: confirmed={}; stop={}; assertions={}",
                    index + 1,
                    bug["confirmed"],
                    bug["stop"],
                    bug["violations"]
                );
            }
            for replay in selected["replays"].as_array().into_iter().flatten() {
                show_replay(replay);
            }
            for assertion in selected["never_satisfied"].as_array().into_iter().flatten() {
                println!("unmet reachability assertion: {assertion}");
            }
            if !bugs.is_empty() {
                println!(
                    "show a finding: harmony show {} --finding 1",
                    path.display()
                );
            }
        } else if selected.is_object() {
            println!(
                "exit: {}; terminal: {}; console: serial.log",
                selected["container_rc"], selected["terminal"]
            );
        } else {
            println!("no result recorded");
        }
    }
    Ok(0)
}

#[cfg(feature = "frontend")]
pub fn timeline(selection: Selection, point: Point, json: bool) -> Result<u8> {
    let path = crate::runs::locate(&selection.run)?;
    let manifest = Manifest::read(&path)?;
    let mut witness = recorded_witness(&manifest, selection.finding)?;
    if point.specified() {
        let (_, boundaries, anchor) = trajectory(&witness, selection.finding.is_some());
        let step = point.resolve(&boundaries, anchor)?;
        witness.timeline.retain(|entry| entry.step as usize == step);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&witness.timeline)?);
    } else {
        for step in witness.timeline {
            println!(
                "step {}  virtual ns {}  supervisor tick {}  {:?}  {:?}{}",
                step.step,
                step.observation.moment,
                step.observation.ticks,
                step.action,
                step.observation.stop,
                if step.settlement {
                    " (recovery check)"
                } else {
                    ""
                }
            );
            println!(
                "  observed: hooks {}/{} started/finished; event kills {}; event parks {}",
                step.observation.hooks_started,
                step.observation.hooks_finished,
                step.observation.event_kill_fires,
                step.observation.event_park_fires
            );
            for assertion in step.observation.violations() {
                println!("  violation: {assertion}");
            }
        }
    }
    Ok(0)
}

#[cfg(feature = "frontend")]
pub fn logs(
    selection: Selection,
    point: Point,
    contains: Option<String>,
    json: bool,
) -> Result<u8> {
    let path = crate::runs::locate(&selection.run)?;
    let manifest = Manifest::read(&path)?;
    let mut sections = Vec::new();
    let filter = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| contains.as_ref().is_none_or(|needle| line.contains(needle)))
            .map(str::to_owned)
            .collect()
    };
    if manifest.bundle.is_none() && path.join("serial.log").is_file() && selection.finding.is_none()
    {
        if point.specified() {
            return Err("command logs have no action boundaries; omit point selectors".into());
        }
        let serial = fs::read(path.join("serial.log"))?;
        if manifest.bundle.is_none() {
            if let Some(expected) = manifest.record.payload["outcome"]["serial_sha256"].as_str() {
                if crate::runtime::digest(&serial) != expected {
                    return Err("recorded serial.log has changed".into());
                }
            } else if manifest.status == "complete" {
                return Err("completed command has no saved serial digest".into());
            }
        }
        sections.push(serde_json::json!({"source":"console","step":null,"lines":filter(&String::from_utf8(serial)?)}));
    } else {
        let witness = match recorded_witness(&manifest, selection.finding) {
            Ok(witness) => Some(witness),
            Err(_)
                if selection.finding.is_none()
                    && !point.specified()
                    && path.join("terminal.log").is_file() =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        if let Some(witness) = witness {
            let at = if point.specified() {
                let (_, boundaries, anchor) = trajectory(&witness, selection.finding.is_some());
                Some(point.resolve(&boundaries, anchor)? as u64)
            } else {
                None
            };
            for step in witness
                .timeline
                .into_iter()
                .filter(|step| at.is_none_or(|at| at == step.step))
            {
                sections.push(serde_json::json!({"source":"console","step":step.step,"lines":filter(&step.console)}));
            }
        }
    }
    if selection.finding.is_none() && !point.specified() && path.join("terminal.log").is_file() {
        sections.push(serde_json::json!({"source":"terminal","step":null,"lines":filter(&String::from_utf8_lossy(&fs::read(path.join("terminal.log"))?))}));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&sections)?);
    } else {
        for section in sections {
            println!(
                "--- {} step {} ---",
                section["source"].as_str().unwrap_or_default(),
                section["step"]
            );
            for line in section["lines"].as_array().into_iter().flatten() {
                println!("{}", line.as_str().unwrap_or_default());
            }
        }
    }
    Ok(0)
}

#[cfg(test)]
#[cfg(feature = "frontend")]
mod tests {
    use super::*;
    use faults_workload::FaultOperation;
    use std::num::NonZeroU16;

    #[test]
    fn exact_replay_rejects_changed_timeline_and_console() {
        use faults_workload::{package::ReplayStep, target::FaultObservations};
        let mut expected =
            ReplaySummary::from_observation(&FaultObservations::default(), Some([7; 32]), 0, 0);
        expected.timeline = vec![ReplayStep {
            step: 0,
            action: None,
            observation: FaultObservations::default(),
            console: "ready".into(),
            settlement: false,
        }];
        let mut actual = expected.clone();
        actual.timeline[0].console = "different output".into();
        assert!(!equivalent(&expected, &actual));
        actual = expected.clone();
        actual.timeline[0].observation.moment = 1;
        assert!(!equivalent(&expected, &actual));
        actual = expected.clone();
        actual.run = 2;
        actual.guest_horizons = 1;
        assert!(equivalent(&expected, &actual));
    }

    fn saved_replay_record(path: &std::path::Path, digests: &[Option<[u8; 32]>], status: &str) {
        use faults_workload::{package::Report, target::FaultObservations};

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
        for (index, digest) in digests.iter().enumerate() {
            let mut summary =
                ReplaySummary::from_observation(&FaultObservations::default(), *digest, 0, 0);
            summary.run = (index + 1) as u32;
            report.replays.push(summary);
        }
        report.write(path).unwrap();

        let mut config = crate::config::Config::default();
        config.runner.kind = "test".into();
        config.workload.package = "faults".into();
        let mut record = crate::runs::Manifest::new(config, "run").unwrap();
        record.store(path, "kernel", b"").unwrap();
        record.store(path, "initramfs", b"").unwrap();
        record.status = status.into();
        record.payload = serde_json::json!({
            "bundle":"node app \"/bin/app\"\n","actions":[],"settle":true,
            "outcome":serde_json::to_value(&report).unwrap()
        });
        record.save(path).unwrap();
    }

    #[test]
    fn saved_replay_refuses_disagreeing_recorded_repeats_before_execution() {
        let dir = tempfile::tempdir().unwrap();
        saved_replay_record(dir.path(), &[Some([7; 32]), Some([8; 32])], "complete");

        let error = replay(
            Selection {
                run: dir.path().display().to_string(),
                finding: None,
            },
            Destination {
                out: Some(dir.path().join("child")),
                name: None,
            },
            1,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("saved repeats disagree"), "{error}");
        assert!(!dir.path().join("child").exists());
    }

    #[test]
    fn saved_replay_refuses_failed_recording_before_execution() {
        let dir = tempfile::tempdir().unwrap();
        saved_replay_record(dir.path(), &[Some([7; 32]), Some([7; 32])], "failed");
        let original = Manifest::read(dir.path()).unwrap();
        assert!(recorded_witness(&original, None).is_ok());

        let error = replay(
            Selection {
                run: dir.path().display().to_string(),
                finding: None,
            },
            Destination {
                out: Some(dir.path().join("child")),
                name: None,
            },
            1,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("failed or incomplete recording"), "{error}");
        assert!(!dir.path().join("child").exists());
    }

    #[test]
    fn divergent_and_uml_saved_repeats_remain_inspectable() {
        for digests in [[Some([7; 32]), Some([8; 32])], [None, None]] {
            let dir = tempfile::tempdir().unwrap();
            saved_replay_record(dir.path(), &digests, "complete");
            let selection = Selection {
                run: dir.path().display().to_string(),
                finding: None,
            };
            assert!(timeline(selection.clone(), Point::default(), true).is_ok());
            assert!(logs(selection, Point::default(), None, true).is_ok());
        }
    }

    #[test]
    fn command_replay_uses_the_outcome_from_the_verified_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let recorded = serde_json::json!({"container_rc": 0, "serial_sha256": "first"});
        let replacement = serde_json::json!({"container_rc": 1, "serial_sha256": "second"});
        fs::write(
            dir.path().join("run.json"),
            serde_json::to_vec(&recorded).unwrap(),
        )
        .unwrap();
        let mut config = crate::config::Config::default();
        config.runner.kind = "test".into();
        config.workload.package = "faults".into();
        let mut manifest = crate::runs::Manifest::new(config, "command").unwrap();
        manifest.payload = serde_json::json!({
            "bundle": null, "actions": [], "settle": true, "outcome": recorded
        });
        manifest.save(dir.path()).unwrap();
        let original = Manifest::read(dir.path()).unwrap();
        original.verify(dir.path()).unwrap();

        fs::write(
            dir.path().join("run.json"),
            serde_json::to_vec(&replacement).unwrap(),
        )
        .unwrap();
        assert!(original.verify(dir.path()).is_err());
        assert!(!command_replay_matches(&original, replacement));
    }

    #[test]
    fn finding_selection_uses_the_report_from_the_verified_manifest() {
        use faults_workload::{
            package::{BugSummary, Report},
            target::{FaultObservations, FaultStop},
        };

        let dir = tempfile::tempdir().unwrap();
        let options = faults_workload::Options {
            seed: 0,
            executions: 1,
            ram_mib: 1024,
            knobs: Vec::new(),
            wall_seconds: None,
            output: dir.path().to_path_buf(),
            uml_profile: None,
            root: None,
            checkpoint_owner: None,
        };
        let mut report = Report::new(
            "search",
            &faults_workload::Artifacts {
                kernel: Vec::new(),
                initramfs: Vec::new(),
            },
            "runner".into(),
            &options,
            1,
        );
        report.bugs.push(BugSummary {
            execution: 1,
            actions: Vec::new(),
            stop: FaultStop::Assertion { point: 2 },
            violations: vec!["2".into()],
            sometimes: Vec::new(),
            state_hash: String::new(),
            state_hash_encoding: StateHashEncoding::EngineDigest,
            confirmed: false,
            replay: Some(ReplaySummary::from_observation(
                &FaultObservations::default(),
                Some([7; 32]),
                1,
                1,
            )),
        });
        report.write(dir.path()).unwrap();
        let mut config = crate::config::Config::default();
        config.runner.kind = "test".into();
        config.workload.package = "faults".into();
        let mut manifest = crate::runs::Manifest::new(config, "search").unwrap();
        manifest.payload = serde_json::json!({
            "bundle": "node app \"/bin/app\"\n", "actions": [], "settle": true,
            "outcome": serde_json::to_value(&report).unwrap()
        });
        manifest.save(dir.path()).unwrap();
        let original = Manifest::read(dir.path()).unwrap();
        original.verify(dir.path()).unwrap();

        let recorded_hash = report.bugs[0].replay.as_ref().unwrap().state_hash.clone();
        report.bugs[0].confirmed = true;
        report.bugs[0]
            .actions
            .push(faults_workload::FaultAction::new(
                FaultOperation::Wait(NonZeroU16::new(1).unwrap()),
                NonZeroU16::new(1).unwrap(),
            ));
        report.bugs[0].replay.as_mut().unwrap().state_hash = "changed".into();
        report.write(dir.path()).unwrap();
        assert!(original.verify(dir.path()).is_err());
        assert!(!recorded_report(&original).unwrap().bugs[0].confirmed);
        assert!(selected_actions(&original, Some(1)).unwrap().is_empty());
        assert_eq!(
            recorded_witness(&original, Some(1)).unwrap().state_hash,
            recorded_hash
        );
    }

    #[test]
    fn replay_does_not_certify_an_unconfirmed_finding() {
        use faults_workload::{
            package::{BugSummary, Report},
            target::{FaultObservations, FaultStop},
        };

        let dir = tempfile::tempdir().unwrap();
        let options = faults_workload::Options {
            seed: 0,
            executions: 1,
            ram_mib: 1024,
            knobs: Vec::new(),
            wall_seconds: None,
            output: dir.path().to_path_buf(),
            uml_profile: None,
            root: None,
            checkpoint_owner: None,
        };
        let mut report = Report::new(
            "search",
            &faults_workload::Artifacts {
                kernel: Vec::new(),
                initramfs: Vec::new(),
            },
            "runner".into(),
            &options,
            1,
        );
        report.bugs.push(BugSummary {
            execution: 1,
            actions: Vec::new(),
            stop: FaultStop::Assertion { point: 2 },
            violations: vec!["2".into()],
            sometimes: Vec::new(),
            state_hash: String::new(),
            state_hash_encoding: StateHashEncoding::EngineDigest,
            confirmed: false,
            replay: Some(ReplaySummary::from_observation(
                &FaultObservations::default(),
                Some([7; 32]),
                1,
                1,
            )),
        });
        report.write(dir.path()).unwrap();
        let mut shared = crate::config::Config::default();
        shared.runner.kind = "test".into();
        shared.workload.package = "faults".into();
        let mut record = crate::runs::Manifest::new(shared, "search").unwrap();
        record.store(dir.path(), "kernel", b"").unwrap();
        record.store(dir.path(), "initramfs", b"").unwrap();
        record.status = "complete".into();
        record.payload = serde_json::json!({
            "bundle":"node app \"/bin/app\"\n","actions":[],"settle":true,
            "outcome":serde_json::to_value(&report).unwrap()
        });
        record.save(dir.path()).unwrap();

        let error = replay(
            Selection {
                run: dir.path().display().to_string(),
                finding: Some(1),
            },
            Destination {
                out: Some(dir.path().join("child")),
                name: None,
            },
            1,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not confirmed"), "{error}");
        assert!(!dir.path().join("child").exists());

        report.bugs[0].confirmed = true;
        report.write(dir.path()).unwrap();
        record.payload["outcome"] = serde_json::to_value(&report).unwrap();
        record.save(dir.path()).unwrap();
        let later_error = replay(
            Selection {
                run: dir.path().display().to_string(),
                finding: Some(1),
            },
            Destination {
                out: Some(dir.path().join("confirmed-child")),
                name: None,
            },
            1,
        )
        .unwrap_err()
        .to_string();
        assert!(!later_error.contains("not confirmed"), "{later_error}");
        assert!(dir.path().join("confirmed-child").exists());
    }
    #[test]
    fn finding_anchor_includes_recovery_and_precedes_later_recorded_actions() {
        use faults_workload::{
            package::ReplayStep,
            target::{FaultObservations, FaultStop},
        };
        let mut replay =
            ReplaySummary::from_observation(&FaultObservations::default(), Some([0; 32]), 1, 0);
        replay.settle_actions = 2;
        replay.timeline = (0..=3)
            .map(|step| ReplayStep {
                step,
                action: (step != 0).then(|| {
                    FaultAction::new(FaultOperation::Wait(NonZeroU16::MIN), NonZeroU16::MIN)
                }),
                observation: FaultObservations {
                    moment: step * 1_000_000,
                    stop: if step >= 2 {
                        FaultStop::Crash
                    } else {
                        FaultStop::default()
                    },
                    ..Default::default()
                },
                console: String::new(),
                settlement: step > 1,
            })
            .collect();
        let (actions, boundaries, anchor) = trajectory(&replay, true);
        assert_eq!(anchor, 2);
        assert_eq!(actions.len(), 3);
        assert_eq!(
            Point {
                rewind: Some(1),
                ..Default::default()
            }
            .resolve(&boundaries, anchor)
            .unwrap(),
            1
        );
    }

    #[test]
    fn unavailable_state_hash_never_verifies_full_replay() {
        use faults_workload::target::FaultObservations;

        let observation = FaultObservations::default();
        let known = ReplaySummary::from_observation(&observation, Some([7; 32]), 1, 1);
        let unknown = ReplaySummary::from_observation(&observation, None, 1, 1);
        assert!(equivalent(&known, &known));
        assert!(!equivalent(&unknown, &unknown));
        assert!(!equivalent(&known, &unknown));
    }
}
