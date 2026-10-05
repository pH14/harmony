// SPDX-License-Identifier: AGPL-3.0-or-later

use super::runs::Manifest;
use crate::{config::Result, runs::Destination};
use faults_workload::{
    FaultAction, FaultOperation,
    package::{self, ReplaySummary, Report},
};
use std::{fs, num::NonZeroU16, path::Path};

pub use crate::selection::Selection;
use crate::selection::{Point, millis};

fn report(path: &Path) -> Result<Report> {
    Ok(serde_json::from_slice(&fs::read(
        path.join("report.json"),
    )?)?)
}

fn bug_index(bug: usize, length: usize) -> Result<usize> {
    bug.checked_sub(1)
        .filter(|i| *i < length)
        .ok_or_else(|| format!("bug {bug} does not exist; this run has {length} findings").into())
}

pub fn selected_actions(
    path: &Path,
    manifest: &Manifest,
    bug: Option<usize>,
) -> Result<Vec<FaultAction>> {
    if let Some(bug) = bug {
        let report = report(path)?;
        Ok(report.bugs[bug_index(bug, report.bugs.len())?]
            .actions
            .clone())
    } else if manifest.mode == "search" {
        Err(
            "select a finding with --finding NUMBER; use resume RUN to continue the whole search"
                .into(),
        )
    } else {
        Ok(manifest.actions.clone())
    }
}

fn witness(path: &Path, bug: Option<usize>) -> Result<ReplaySummary> {
    let report = report(path)?;
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

fn equivalent(expected: &ReplaySummary, actual: &ReplaySummary) -> bool {
    expected.state_hash == actual.state_hash
        && expected.state_hash_encoding == actual.state_hash_encoding
        && expected.stop == actual.stop
        && expected.violations == actual.violations
        && expected.sometimes == actual.sometimes
        && expected.actions_applied == actual.actions_applied
        && expected.settle_actions == actual.settle_actions
        && expected.settle_ticks == actual.settle_ticks
        && expected.event_kill_fires == actual.event_kill_fires
        && expected.event_park_fires == actual.event_park_fires
        && expected.check == actual.check
}

pub fn replay(selection: Selection, destination: Destination, repeat: u32) -> Result<u8> {
    if repeat == 0 {
        return Err("repeat must be positive".into());
    }
    let source = crate::runs::locate(&selection.run)?;
    let original = Manifest::read(&source)?;
    original.verify(&source)?;
    let recorded = if original.bundle.is_some() {
        Some((
            selected_actions(&source, &original, selection.finding)?,
            witness(&source, selection.finding)?,
        ))
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
            crate::oci::execute(
                &c.runtime(),
                c.seed,
                &c.knobs,
                c.wall_seconds,
                &artifacts.kernel,
                &artifacts.initramfs,
                &out,
                false,
            )?;
            let old: serde_json::Value =
                serde_json::from_slice(&fs::read(source.join("run.json"))?)?;
            let new: serde_json::Value = serde_json::from_slice(&fs::read(out.join("run.json"))?)?;
            if !crate::oci::equivalent_record(old, new, manifest.config.backend) {
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
                &manifest.options(&out),
                original.settle,
            )?;
            if replay
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

pub fn branch(request: crate::adapters::Request) -> Result<u8> {
    let selection = request.selection.ok_or("select a run")?;
    let source = crate::runs::locate(&selection.run)?;
    let original = Manifest::read(&source)?;
    original.verify(&source)?;
    if original.bundle.is_none() {
        return Err("branching requires a supervised application scenario".into());
    }
    let recorded = witness(&source, selection.finding)?;
    let (actions, boundaries, anchor) = trajectory(&recorded, selection.finding.is_some());
    let cut = request.point.resolve(&boundaries, anchor)?;
    if cut > actions.len() {
        return Err("selected boundary has no reproducible input prefix".into());
    }
    let quantum = actions
        .get(cut.saturating_sub(1))
        .or(actions.first())
        .map_or(NonZeroU16::MIN, |action| action.coverage_quantum);
    let mut continuation = actions[..cut].to_vec();
    let mut injections = Vec::new();
    for name in &request.interventions {
        injections.extend(original.config.interventions.get(name).ok_or_else(|| format!("unknown intervention {name:?}; define workload.options.interventions.{name}"))?.clone());
    }
    if let Some(text) = &request.intervention_toml {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Plan {
            actions: Vec<super::config::Intervention>,
        }
        injections.extend(toml::from_str::<Plan>(text)?.actions);
    }
    for injection in &injections {
        continuation.push(injection_action(
            injection,
            original.bundle.as_deref().unwrap_or_default(),
            &original,
            quantum,
        )?);
    }
    if let Some(path) = request.actions {
        continuation
            .extend(faults_workload::parse_recorded_input(&fs::read_to_string(path)?)?.actions);
    }
    if !request.stop {
        continuation.extend_from_slice(&actions[cut..]);
    }
    let out = request.destination.create()?;
    let mut manifest =
        original.inherit(&source, &out, if !request.stop { "run" } else { "branch" })?;
    manifest.actions = continuation;
    manifest.settle = !request.stop;
    manifest.save(&out)?;
    println!(
        "branch point: after action {cut}; virtual offset {} ms from setup",
        boundaries
            .iter()
            .find(|(step, _)| *step == cut)
            .map(|(_, moment)| moment
                .saturating_sub(boundaries.first().map_or(0, |(_, moment)| *moment))
                / 1_000_000)
            .unwrap_or_default()
    );
    let result = package::execute_actions(
        &manifest.fault_artifacts(&out)?,
        &manifest.actions,
        1,
        &manifest.options(&out),
        !request.stop,
    )
    .map(|report| super::workflow::report_code(&report));
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
    path: &Path,
    manifest: &Manifest,
    selection: &Selection,
    point: &Point,
) -> Result<Vec<FaultAction>> {
    if !point.specified() && selection.finding.is_none() {
        return selected_actions(path, manifest, None);
    }
    let recorded = witness(path, selection.finding)?;
    let (actions, boundaries, anchor) = trajectory(&recorded, selection.finding.is_some());
    let cut = point.resolve(&boundaries, anchor)?;
    if cut > actions.len() {
        return Err("point has no reproducible input prefix".into());
    }
    eprintln!("selected step {cut}");
    Ok(actions[..cut].to_vec())
}

fn injection_action(
    action: &super::config::Intervention,
    bundle: &str,
    manifest: &Manifest,
    quantum: NonZeroU16,
) -> Result<FaultAction> {
    use super::config::Intervention;
    let (kind, target, duration) = match action {
        Intervention::Kill { node, duration } => ("kill", node.as_str(), duration.as_str()),
        Intervention::Pause { node, duration } => ("pause", node.as_str(), duration.as_str()),
        Intervention::Restart { node, duration } => ("restart", node.as_str(), duration.as_str()),
        Intervention::Wait { duration } => ("wait", "", duration.as_str()),
        Intervention::Hook { name, duration } => ("hook", name.as_str(), duration.as_str()),
    };
    let ms = millis(duration)?;
    if ms % 10 != 0 {
        return Err("fault durations must be whole 10ms ticks".into());
    }
    let ticks = NonZeroU16::new(u16::try_from(ms / 10)?).ok_or("zero fault duration")?;
    let node = || -> Result<u16> {
        let nodes: Vec<_> = bundle
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                (fields.next() == Some("node"))
                    .then(|| fields.next())
                    .flatten()
            })
            .collect();
        nodes
            .iter()
            .position(|name| *name == target)
            .map(|n| n as u16)
            .ok_or_else(|| format!("unknown node {target:?}; nodes: {}", nodes.join(", ")).into())
    };
    let operation = match kind {
        "wait" => FaultOperation::Wait(ticks),
        "kill" => FaultOperation::Kill(node()?, ticks),
        "pause" => FaultOperation::Pause(node()?, ticks),
        "restart" => FaultOperation::Restart(node()?, ticks),
        "hook" => {
            let id =
                if let Some(index) = manifest.config.hooks.keys().position(|name| name == target) {
                    index as u32 + 1
                } else {
                    target.parse::<u32>()?
                };
            let vocabulary = FaultVocabulary::parse(bundle)?;
            if !vocabulary.hooks().contains(&id) {
                return Err("unknown hook".into());
            }
            FaultOperation::Hook(id, ticks)
        }
        _ => return Err(format!("unknown intervention {kind:?}").into()),
    };
    Ok(FaultAction::new(operation, quantum))
}

use faults_workload::FaultVocabulary;

pub fn inspect(selection: Selection, json: bool) -> Result<u8> {
    let path = crate::runs::locate(&selection.run)?;
    let manifest = Manifest::read(&path)?;
    let report_path = if path.join("report.json").exists() {
        path.join("report.json")
    } else {
        path.join("run.json")
    };
    let report: serde_json::Value = if report_path.is_file() {
        serde_json::from_slice(&fs::read(report_path)?)?
    } else {
        serde_json::Value::Null
    };
    let selected = if let Some(bug) = selection.finding {
        let bugs = report["bugs"]
            .as_array()
            .ok_or("this run has no findings")?;
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
                replay["state_hash"]
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
                "timeline: harmony timeline {} --finding {bug}\nreplay: harmony replay {} --finding {bug}",
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
                    "inspect a finding: harmony inspect {} --finding 1",
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

pub fn timeline(selection: Selection, point: Point, json: bool) -> Result<u8> {
    let path = crate::runs::locate(&selection.run)?;
    let mut witness = witness(&path, selection.finding)?;
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

pub fn logs(selection: Selection, point: Point, contains: Option<String>) -> Result<u8> {
    let path = crate::runs::locate(&selection.run)?;
    let filter = |text: &str| {
        for line in text.lines() {
            if contains.as_ref().is_none_or(|needle| line.contains(needle)) {
                println!("{line}");
            }
        }
    };
    if path.join("serial.log").is_file() && selection.finding.is_none() {
        if point.specified() {
            return Err("command logs have no action boundaries; omit point selectors".into());
        }
        filter(&fs::read_to_string(path.join("serial.log"))?);
    } else {
        let witness = witness(&path, selection.finding)?;
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
            println!(
                "--- step {} (console captured at this boundary) ---",
                step.step
            );
            filter(&step.console);
        }
    }
    Ok(0)
}

pub fn findings(selection: Selection, json: bool) -> Result<u8> {
    let path = crate::runs::locate(&selection.run)?;
    let report = report(&path)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report.bugs)?);
    } else {
        for (index, bug) in report.bugs.iter().enumerate() {
            println!(
                "finding {}: confirmed={} stop={:?} assertions={:?}",
                index + 1,
                bug.confirmed,
                bug.stop,
                bug.violations
            );
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finding_anchor_includes_recovery_and_precedes_later_recorded_actions() {
        use faults_workload::{
            package::ReplayStep,
            target::{FaultObservations, FaultStop},
        };
        let mut replay =
            ReplaySummary::from_observation(&FaultObservations::default(), [0; 32], 1, 0);
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
    fn typed_interventions_resolve_names_and_reject_unrepresentable_durations() {
        let manifest =
            Manifest::new(super::super::config::Config::default(), "branch", None).unwrap();
        let bundle = "node alpha /app\nnode beta /app\n";
        let action = super::super::config::Intervention::Kill {
            node: "beta".into(),
            duration: "100ms".into(),
        };
        let result =
            injection_action(&action, bundle, &manifest, NonZeroU16::new(64).unwrap()).unwrap();
        assert_eq!(
            result.operation,
            FaultOperation::Kill(1, NonZeroU16::new(10).unwrap())
        );
        assert_eq!(result.coverage_quantum.get(), 64);
        assert!(
            injection_action(
                &super::super::config::Intervention::Pause {
                    node: "beta".into(),
                    duration: "1ms".into()
                },
                bundle,
                &manifest,
                NonZeroU16::MIN
            )
            .is_err()
        );
    }
}
