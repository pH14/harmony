// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    adapters::{Operation, Request},
    config::Result,
    runs::{Manifest, locate},
};
use std::fs;
pub fn saved(mut request: Request) -> Result<u8> {
    let path = locate(
        &request
            .selection
            .as_ref()
            .ok_or("select a search or branch")?
            .run,
    )?;
    let manifest = Manifest::read(&path)?;
    if manifest.config.runner.kind != "consonance"
        && !matches!(
            request.operation,
            Operation::Inspect | Operation::Timeline | Operation::Logs
        )
    {
        manifest.verify(&path)?;
    }
    if request.operation == Operation::Search && manifest.mode != "branch" {
        return Err(
            "search --from requires a branch; use branch NAME to choose a starting point".into(),
        );
    }
    request.config = manifest.config;
    crate::adapters::dispatch(request)
}
pub fn archive_entries(run: &str) -> Result<u8> {
    let given = std::path::Path::new(run);
    let explicit = given.is_file();
    let root = if explicit {
        given
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or("a checkpoint path must sit in its run's checkpoints directory")?
            .to_path_buf()
    } else {
        locate(run)?
    };
    let manifest = Manifest::read(&root)?;
    let (checkpoint, expected_sha256) = if explicit {
        (given.to_path_buf(), None)
    } else {
        let (pinned, _) = crate::runs::checkpoint_record_verified(&root, &manifest)?;
        (pinned.path, Some(pinned.sha256))
    };
    match crate::adapters::select(&manifest.config.workload)?.name() {
        "faults" => write_archive_entries(
            searcher::search::campaign::checkpoint_archive_entries::<
                faults_workload::campaign::FaultWorkload,
            >(&checkpoint, expected_sha256.as_deref())?,
            &mut std::io::stdout().lock(),
        ),
        name => Err(format!("workload {name} has no archive entry listing").into()),
    }
}
fn archive_entry_line<A, K, M>(
    entry: searcher::search::archive::ArchiveEntryReport<A, K, M>,
    active: bool,
) -> serde_json::Value
where
    A: Ord + serde::Serialize,
    K: serde::Serialize,
    M: serde::Serialize,
{
    serde_json::json!({
        "id": entry.id,
        "parent_id": entry.parent_id,
        "created_execution": entry.created_execution,
        "depth": entry.input.actions.len(),
        "active": active,
        "key": entry.key,
        "milestones": entry.milestones,
        "selector": entry.selector,
        "shrink_total": entry.shrink_total,
        "cost_total": entry.cost_total,
        "actions": entry.input.actions,
    })
}
fn write_archive_entries<A, K, M>(
    entries: Vec<(searcher::search::archive::ArchiveEntryReport<A, K, M>, bool)>,
    out: &mut impl std::io::Write,
) -> Result<u8>
where
    A: Ord + serde::Serialize,
    K: serde::Serialize,
    M: serde::Serialize,
{
    let mut out = std::io::BufWriter::new(out);
    for (entry, active) in entries {
        serde_json::to_writer(&mut out, &archive_entry_line(entry, active))?;
        std::io::Write::write_all(&mut out, b"\n")?;
    }
    std::io::Write::flush(&mut out)?;
    Ok(0)
}
pub fn list(json: bool) -> Result<u8> {
    let root = std::path::Path::new(".harmony/runs");
    let mut runs = Vec::new();
    if root.exists() {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            match Manifest::read(&entry.path()) { Ok(m) if matches!(m.mode.as_str(), "search" | "branch")=>runs.push(serde_json::json!({"name":entry.file_name().to_string_lossy(),"mode":m.mode,"status":m.status,"workload":m.config.workload.package,"runner":m.config.runner.kind})), Ok(_)=>{}, Err(error)=>eprintln!("cannot read {}: {error}",entry.path().display()) }
        }
    }
    runs.sort_by_key(|v| v["name"].as_str().unwrap_or_default().to_owned());
    if json {
        println!("{}", serde_json::to_string_pretty(&runs)?);
    } else {
        for run in runs {
            println!(
                "{}  {}  {}  {}/{}",
                run["name"].as_str().unwrap_or_default(),
                run["mode"],
                run["status"],
                run["workload"],
                run["runner"]
            );
        }
    }
    Ok(0)
}
pub fn diff(left: &str, right: &str) -> Result<u8> {
    let (left_path, right_path) = (locate(left)?, locate(right)?);
    let left = Manifest::read(&left_path)?;
    let right = Manifest::read(&right_path)?;
    let left_outcome = crate::adapters::select(&left.config.workload)?
        .semantic_outcome(&left_path, &left.payload)?;
    let right_outcome = crate::adapters::select(&right.config.workload)?
        .semantic_outcome(&right_path, &right.payload)?;
    let mut changes = serde_json::Map::new();
    for (name, a, b) in [
        (
            "configuration",
            serde_json::to_value(left.config)?,
            serde_json::to_value(right.config)?,
        ),
        (
            "artifacts",
            serde_json::to_value(left.artifacts)?,
            serde_json::to_value(right.artifacts)?,
        ),
        ("execution", left_outcome, right_outcome),
        ("runner", left.runner_identity, right.runner_identity),
    ] {
        if a != b {
            changes.insert(name.into(), serde_json::json!({"left":a,"right":b}));
        }
    }
    println!("{}", serde_json::to_string_pretty(&changes)?);
    Ok(0)
}
#[cfg(test)]
mod tests {
    use super::*;
    use faults_workload::{FaultAction, FaultOperation};
    use searcher::search::archive::{ArchiveEntryReport, Input};
    use std::num::NonZeroU16;

    #[test]
    fn each_archive_entry_line_carries_the_settle_line_action_path() {
        let ticks = NonZeroU16::new(8).unwrap();
        let paths = [
            vec![FaultAction::new(
                FaultOperation::Wait(ticks),
                NonZeroU16::MIN,
            )],
            vec![
                FaultAction::new(FaultOperation::Wait(ticks), NonZeroU16::MIN),
                FaultAction::new(FaultOperation::Hook(5, ticks), NonZeroU16::MIN),
            ],
        ];
        let entries = paths
            .iter()
            .enumerate()
            .map(|(id, actions)| {
                (
                    ArchiveEntryReport {
                        id: id as u64 + 1,
                        parent_id: id.checked_sub(1).map(|parent| parent as u64 + 1),
                        created_execution: 2,
                        input: Input {
                            actions: actions.clone(),
                        },
                        key: (),
                        milestones: (),
                        snapshot_identity_sha256: None,
                        selector: None,
                        shrink_total: None,
                        cost_total: None,
                    },
                    id == 0,
                )
            })
            .collect();
        let mut out = Vec::new();
        assert_eq!(write_archive_entries(entries, &mut out).unwrap(), 0);
        let text = String::from_utf8(out).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), paths.len());
        for (index, (line, actions)) in lines.iter().zip(&paths).enumerate() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["id"], index as u64 + 1);
            assert_eq!(value["depth"], actions.len());
            assert_eq!(value["active"], index == 0);
            assert_eq!(value["actions"], serde_json::to_value(actions).unwrap());
            assert_eq!(
                &faults_workload::parse_recorded_input(line).unwrap().actions,
                actions
            );
        }
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[1]).unwrap()["parent_id"],
            1
        );
    }
}
