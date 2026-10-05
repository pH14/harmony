// SPDX-License-Identifier: AGPL-3.0-or-later

use searcher::search::draw::draw_mixture_from_identifier;
use serde::Deserialize;
use std::{error::Error, io::Read};
use tiny_worlds::{Keep, Scale, SearchSettings, Workload, run_kept, run_scaled, worlds::World};

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
    #[serde(default)]
    workers: Option<u32>,
    #[serde(default)]
    search: Option<SearchRequest>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchRequest {
    #[serde(default)]
    mixture: Option<String>,
    #[serde(default)]
    stop_on_objective: Option<bool>,
}
fn main() -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    std::io::stdin().take(16_385).read_to_string(&mut input)?;
    if input.len() > 16_384 {
        return Err("request exceeds 16KB".into());
    }
    let request: Request = serde_json::from_str(&input)?;
    let mut settings = SearchSettings::default();
    if let Some(search) = &request.search {
        settings.mixture = search
            .mixture
            .as_deref()
            .map(draw_mixture_from_identifier)
            .transpose()?;
        settings.stop_on_objective = search.stop_on_objective;
    }
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
        if request.workers.is_some() {
            return Err("scaled runs set workers in scale".into());
        }
        if request.verify || request.keep != Keep::Portfolio {
            return Err("scaled runs need verify false and keep portfolio".into());
        }
        let mut progress = std::io::LineWriter::new(std::io::stderr().lock());
        println!(
            "{}",
            run_scaled(
                &workload,
                request.seed,
                request.work_budget,
                &mut progress,
                settings
            )?
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
            request.verify,
            request.workers.unwrap_or(1),
            settings
        )?
    );
    Ok(())
}
