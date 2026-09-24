// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::Deserialize;
use std::{error::Error, io::Read};
use tiny_worlds::{Workload, resource::Config, run};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    config: Config,
    seed: u64,
    work_budget: u64,
    omit_stock: bool,
    verify: bool,
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
        omit_stock: request.omit_stock,
    };
    workload.config.validate()?;
    if !workload.config.reachable()? {
        return Err("world objective is unreachable".into());
    }
    println!(
        "{}",
        run(&workload, request.seed, request.work_budget, request.verify)?
    );
    Ok(())
}
