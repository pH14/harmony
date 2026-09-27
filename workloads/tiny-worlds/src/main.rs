// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::Deserialize;
use std::{error::Error, io::Read};
use tiny_worlds::{Keep, Scale, Workload, run_kept, run_scaled, worlds::World};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    config: World,
    seed: u64,
    work_budget: u64,
    broken: bool,
    verify: bool,
    keep: Keep,
    #[serde(default)]
    scale: Option<Scale>,
}
fn main() -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    std::io::stdin().take(16_385).read_to_string(&mut input)?;
    if input.len() > 16_384 {
        return Err("request exceeds 16KB".into());
    }
    let request: Request = serde_json::from_str(&input)?;
    let workload = Workload {
        config: request.config,
        broken: request.broken,
        scale: request.scale,
    };
    workload.config.validate()?;
    if !workload.config.reachable()? {
        return Err("world objective is unreachable".into());
    }
    if request.scale.is_some() {
        if request.verify || request.keep != Keep::Portfolio {
            return Err("scaled runs need verify false and keep portfolio".into());
        }
        let mut progress = std::io::LineWriter::new(std::io::stderr().lock());
        println!(
            "{}",
            run_scaled(&workload, request.seed, request.work_budget, &mut progress)?
        );
        return Ok(());
    }
    println!(
        "{}",
        run_kept(
            &workload,
            request.keep,
            request.seed,
            request.work_budget,
            request.verify
        )?
    );
    Ok(())
}
