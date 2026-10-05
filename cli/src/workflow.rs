// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    adapters::{Operation, Request},
    config::Result,
    runs::{Manifest, locate},
};
use std::fs;
pub fn saved(mut request: Request) -> Result<u8> {
    let path = locate(&request.selection.as_ref().ok_or("select a run")?.run)?;
    let manifest = Manifest::read(&path)?;
    if !matches!(
        request.operation,
        Operation::Inspect | Operation::Findings | Operation::Timeline | Operation::Logs
    ) {
        manifest.verify(&path)?;
    }
    request.config = manifest.config;
    crate::adapters::dispatch(request)
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
            match Manifest::read(&entry.path()) { Ok(m)=>runs.push(serde_json::json!({"name":entry.file_name().to_string_lossy(),"mode":m.mode,"status":m.status,"workload":m.config.workload.package,"runner":m.config.runner.kind})), Err(error)=>eprintln!("cannot read {}: {error}",entry.path().display()) }
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
        ("execution", left.payload, right.payload),
        ("runner", left.runner_identity, right.runner_identity),
    ] {
        if a != b {
            changes.insert(name.into(), serde_json::json!({"left":a,"right":b}));
        }
    }
    println!("{}", serde_json::to_string_pretty(&changes)?);
    Ok(0)
}
