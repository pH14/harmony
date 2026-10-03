// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    config::Result,
    runs::{Destination, Manifest},
};
use faults_workload::{
    FaultAction, FaultOperation,
    package::{self, ReplaySummary, Report},
};
use std::{
    fs,
    num::NonZeroU16,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, clap::Args)]
pub struct Selection {
    pub run: String,
    #[arg(long)]
    pub bug: Option<usize>,
}

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
            "select a finding with --bug NUMBER; use search --resume to continue the whole search"
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
            .ok_or_else(|| "select a finding with --bug NUMBER".into())
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
    if original.config.rom.is_some() {
        return Err(
            "NES replay uses its game-specific replay tool; application replay is supported here"
                .into(),
        );
    }
    let out = destination.create()?;
    let mut manifest = original.inherit(&source, &out, "replay")?;
    let result = (|| -> Result<u8> {
        if original.bundle.is_none() {
            if selection.bug.is_some() || repeat != 1 {
                return Err("command runs have no bug selector and replay once".into());
            }
            manifest.save(&out)?;
            let mut c = manifest.config.clone();
            if manifest.uml_host.is_some() {
                c.uml_profile = Some(out.join("artifacts/uml"));
            }
            let artifacts = manifest.fault_artifacts(&out)?;
            crate::oci::execute(&c, &artifacts.kernel, &artifacts.initramfs, &out, false)?;
            let old: serde_json::Value =
                serde_json::from_slice(&fs::read(source.join("run.json"))?)?;
            let new: serde_json::Value = serde_json::from_slice(&fs::read(out.join("run.json"))?)?;
            if old != new {
                Err("replay diverged from the recorded command execution".into())
            } else {
                Ok(0)
            }
        } else {
            manifest.actions = selected_actions(&source, &original, selection.bug)?;
            let expected = witness(&source, selection.bug)?;
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
    crate::workflow::finish(&out, &mut manifest, result)
}

#[allow(clippy::too_many_arguments)]
pub fn branch(
    selection: Selection,
    destination: Destination,
    at_step: Option<usize>,
    before: Option<String>,
    inject: Vec<String>,
    extra: Option<PathBuf>,
    follow: bool,
) -> Result<u8> {
    let source = crate::runs::locate(&selection.run)?;
    let original = Manifest::read(&source)?;
    original.verify(&source)?;
    if original.bundle.is_none() {
        return Err("branching requires a supervised application scenario".into());
    }
    let actions = selected_actions(&source, &original, selection.bug)?;
    let recorded = witness(&source, selection.bug)?;
    let boundaries = recorded
        .timeline
        .iter()
        .filter(|step| !step.settlement)
        .map(|step| (step.step as usize, step.observation.moment))
        .collect::<Vec<_>>();
    let cut = if let Some(duration) = before.as_deref().filter(|s| !s.ends_with("steps")) {
        rewind_time(&boundaries, millis(duration)?)?
    } else {
        cut(&actions, at_step, before.as_deref())?
    };
    let quantum = actions
        .get(cut.saturating_sub(1))
        .or(actions.first())
        .map_or(NonZeroU16::MIN, |action| action.coverage_quantum);
    let mut continuation = actions[..cut].to_vec();
    for injection in &inject {
        continuation.push(injection_action(
            injection,
            original.bundle.as_deref().unwrap_or_default(),
            &original,
            quantum,
        )?);
    }
    if let Some(path) = extra {
        continuation
            .extend(faults_workload::parse_recorded_input(&fs::read_to_string(path)?)?.actions);
    }
    if follow {
        continuation.extend_from_slice(&actions[cut..]);
    }
    let out = destination.create()?;
    let mut manifest = original.inherit(&source, &out, if follow { "run" } else { "branch" })?;
    manifest.actions = continuation;
    manifest.settle = follow;
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
        follow,
    )
    .map(|report| crate::workflow::report_code(&report));
    crate::workflow::finish(&out, &mut manifest, result)
}

fn cut(actions: &[FaultAction], at: Option<usize>, before: Option<&str>) -> Result<usize> {
    if let Some(at) = at {
        if at <= actions.len() {
            return Ok(at);
        }
        return Err("selected step is past the recorded input".into());
    }
    let Some(before) = before else {
        return Ok(actions.len().saturating_sub(1));
    };
    if let Some(steps) = before.strip_suffix("steps") {
        return actions
            .len()
            .checked_sub(steps.parse::<usize>()?)
            .ok_or_else(|| "rewind precedes setup".into());
    }
    Err("time rewinds require recorded observations".into())
}

fn rewind_time(boundaries: &[(usize, u64)], millis: u64) -> Result<usize> {
    let end = boundaries.last().ok_or("no recorded action boundaries")?.1;
    let nanos = millis
        .checked_mul(1_000_000)
        .ok_or("rewind duration overflow")?;
    let target = end.checked_sub(nanos).ok_or("rewind precedes setup")?;
    boundaries
        .iter()
        .rev()
        .find(|(_, moment)| *moment <= target)
        .map(|(step, _)| *step)
        .ok_or_else(|| "rewind precedes setup".into())
}

fn millis(text: &str) -> Result<u64> {
    let value = if let Some(ms) = text.strip_suffix("ms") {
        ms.parse()?
    } else {
        crate::config::duration(text)?
            .checked_mul(1000)
            .ok_or("duration overflow")?
    };
    if value == 0 {
        return Err("duration must be positive".into());
    }
    Ok(value)
}

fn injection_action(
    text: &str,
    bundle: &str,
    manifest: &Manifest,
    quantum: NonZeroU16,
) -> Result<FaultAction> {
    let words: Vec<_> = text.split_whitespace().collect();
    let (kind, target, duration) = match words.as_slice() {
        ["wait", duration] => ("wait", "", *duration),
        [kind, target] => (*kind, *target, "1s"),
        [kind, target, duration] => (*kind, *target, *duration),
        _ => return Err("use --inject 'kill NODE 1s', 'pause NODE 100ms', 'restart NODE 1s', 'hook NAME 1s' or 'wait 1s'; use --actions for event-site faults".into()),
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
    let selected = if let Some(bug) = selection.bug {
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
        if let Some(error) = manifest.error {
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
        if let Some(bug) = selection.bug {
            println!(
                "finding {bug}: confirmed={}; execution={}",
                selected["confirmed"], selected["execution"]
            );
            show_replay(&selected["replay"]);
            println!(
                "timeline: harmony timeline {} --bug {bug}\nreplay: harmony replay {} --bug {bug}",
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
                    "inspect a finding: harmony inspect {} --bug 1",
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

pub fn timeline(selection: Selection, json: bool) -> Result<u8> {
    let path = crate::runs::locate(&selection.run)?;
    let witness = witness(&path, selection.bug)?;
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

pub fn logs(selection: Selection, at: Option<u64>, contains: Option<String>) -> Result<u8> {
    let path = crate::runs::locate(&selection.run)?;
    let filter = |text: &str| {
        for line in text.lines() {
            if contains.as_ref().is_none_or(|needle| line.contains(needle)) {
                println!("{line}");
            }
        }
    };
    if path.join("serial.log").is_file() && selection.bug.is_none() {
        if at.is_some() {
            return Err("command logs have no action boundaries; omit --at-step".into());
        }
        filter(&fs::read_to_string(path.join("serial.log"))?);
    } else {
        let witness = witness(&path, selection.bug)?;
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

pub fn diff(left: &str, right: &str) -> Result<u8> {
    let left = crate::runs::locate(left)?;
    let right = crate::runs::locate(right)?;
    let a = Manifest::read(&left)?;
    let b = Manifest::read(&right)?;
    let mut differences = serde_json::Map::new();
    for (name, a, b) in [
        (
            "configuration",
            serde_json::to_value(a.config)?,
            serde_json::to_value(b.config)?,
        ),
        (
            "actions",
            serde_json::to_value(a.actions)?,
            serde_json::to_value(b.actions)?,
        ),
        (
            "artifacts",
            serde_json::to_value(a.artifacts)?,
            serde_json::to_value(b.artifacts)?,
        ),
    ] {
        if a != b {
            differences.insert(name.into(), serde_json::json!({"left": a, "right": b}));
        }
    }
    for name in ["report.json", "run.json"] {
        let read = |path: &Path| -> Result<serde_json::Value> {
            Ok(if path.exists() {
                serde_json::from_slice(&fs::read(path)?)?
            } else {
                serde_json::Value::Null
            })
        };
        let a = read(&left.join(name))?;
        let b = read(&right.join(name))?;
        for key in [
            "executions",
            "bug_found",
            "never_satisfied",
            "execution_failures",
            "watchdog_cutoffs",
            "container_rc",
            "serial_sha256",
            "terminal",
        ] {
            if a[key] != b[key] {
                differences.insert(
                    key.into(),
                    serde_json::json!({"left": a[key], "right": b[key]}),
                );
            }
        }
        let outcomes = |value: &serde_json::Value| {
            ["replays", "bugs"].into_iter().flat_map(|field| value[field].as_array().into_iter().flatten())
                .map(|entry| serde_json::json!({"state_hash": entry["state_hash"], "stop": entry["stop"], "violations": entry["violations"], "confirmed": entry["confirmed"]}))
                .collect::<Vec<_>>()
        };
        if outcomes(&a) != outcomes(&b) {
            differences.insert(
                "outcomes".into(),
                serde_json::json!({"left": outcomes(&a), "right": outcomes(&b)}),
            );
        }
    }
    println!("{}", serde_json::to_string_pretty(&differences)?);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn action(ticks: u16) -> FaultAction {
        FaultAction::new(
            FaultOperation::Wait(NonZeroU16::new(ticks).unwrap()),
            NonZeroU16::new(32).unwrap(),
        )
    }
    #[test]
    fn rewinds_resolve_to_recorded_boundaries_without_changing_actions() {
        let actions = vec![action(50), action(100), action(25)];
        assert_eq!(cut(&actions, None, Some("1steps")).unwrap(), 2);
        let boundaries = [
            (0, 100_000_000),
            (1, 600_000_000),
            (2, 1_600_000_000),
            (3, 1_700_000_000),
        ];
        assert_eq!(rewind_time(&boundaries, 1000).unwrap(), 1);
        assert_eq!(rewind_time(&boundaries, 100).unwrap(), 2);
        assert!(rewind_time(&boundaries, 1700).is_err());
        assert!(cut(&actions, None, Some("4steps")).is_err());
        assert!(cut(&actions, Some(4), None).is_err());
    }
    #[test]
    fn injected_faults_resolve_node_names_and_keep_the_recorded_quantum() {
        let manifest = Manifest::new(crate::config::Config::default(), "branch", None).unwrap();
        let bundle = "node alpha /app\nnode beta /app\nhook 3 /debug\n";
        let action = injection_action(
            "kill beta 100ms",
            bundle,
            &manifest,
            NonZeroU16::new(64).unwrap(),
        )
        .unwrap();
        assert_eq!(
            action.operation,
            FaultOperation::Kill(1, NonZeroU16::new(10).unwrap())
        );
        assert_eq!(action.coverage_quantum.get(), 64);
        assert!(injection_action("kill unknown", bundle, &manifest, NonZeroU16::MIN).is_err());
        assert!(injection_action("pause beta 1ms", bundle, &manifest, NonZeroU16::MIN).is_err());
    }
}
