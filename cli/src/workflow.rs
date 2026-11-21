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
    let (root, checkpoint) = if given.is_file() {
        let root = given
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or("a checkpoint path must sit in its run's checkpoints directory")?;
        (root.to_path_buf(), Some(given.to_path_buf()))
    } else {
        (locate(run)?, None)
    };
    let manifest = Manifest::read(&root)?;
    let checkpoint = match checkpoint {
        Some(checkpoint) => checkpoint,
        None => {
            crate::runs::checkpoint_record_verified(&root, &manifest)?
                .0
                .path
        }
    };
    match crate::adapters::select(&manifest.config.workload)?.name() {
        "faults" => print_archive_entries::<faults_workload::campaign::FaultWorkload>(&checkpoint),
        name => Err(format!("workload {name} has no archive entry listing").into()),
    }
}
fn print_archive_entries<G: searcher::search::campaign::Workload>(
    checkpoint: &std::path::Path,
) -> Result<u8> {
    use std::io::Write;
    let entries = searcher::search::campaign::checkpoint_archive_entries::<G>(checkpoint)?;
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for (entry, active) in entries {
        serde_json::to_writer(
            &mut out,
            &serde_json::json!({
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
            }),
        )?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
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
    let left = Manifest::read(&locate(left)?)?;
    let right = Manifest::read(&locate(right)?)?;
    let left_outcome =
        crate::adapters::select(&left.config.workload)?.semantic_outcome(&left.payload);
    let right_outcome =
        crate::adapters::select(&right.config.workload)?.semantic_outcome(&right.payload);
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
